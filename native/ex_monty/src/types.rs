use monty::{MontyObject, OsFunction, ResourceLimits};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use rustler::types::atom::Atom;
use rustler::types::map::MapIterator;
use rustler::types::tuple::get_tuple;
use rustler::{Encoder, Env, NifResult, Term};
use std::time::Duration;

// ── Encoding: MontyObject → Erlang Term ──────────────────────────────────────

pub fn encode_monty_object<'a>(env: Env<'a>, obj: &MontyObject) -> Term<'a> {
    match obj {
        MontyObject::None => rustler::types::atom::nil().encode(env),
        MontyObject::Bool(b) => b.encode(env),
        MontyObject::Int(i) => i.encode(env),
        MontyObject::BigInt(bi) => encode_bigint(env, bi),
        MontyObject::Float(f) => f.encode(env),
        MontyObject::String(s) => s.encode(env),
        MontyObject::Bytes(b) => {
            let tag = Atom::from_str(env, "bytes").unwrap();
            let mut owned = rustler::OwnedBinary::new(b.len()).unwrap();
            owned.as_mut_slice().copy_from_slice(b);
            let binary = owned.release(env);
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), binary.encode(env)])
        }
        MontyObject::Ellipsis => Atom::from_str(env, "ellipsis").unwrap().encode(env),
        MontyObject::List(items) => {
            let terms: Vec<Term> = items.iter().map(|i| encode_monty_object(env, i)).collect();
            terms.encode(env)
        }
        MontyObject::Tuple(items) => {
            let terms: Vec<Term> = items.iter().map(|i| encode_monty_object(env, i)).collect();
            rustler::types::tuple::make_tuple(env, &terms)
        }
        MontyObject::Dict(pairs) => {
            let mut map = rustler::types::map::map_new(env);
            for (k, v) in pairs {
                let key = encode_monty_object(env, k);
                let val = encode_monty_object(env, v);
                map = map.map_put(key, val).unwrap();
            }
            map
        }
        MontyObject::Set(items) | MontyObject::FrozenSet(items) => {
            let members: Vec<Term> = items.iter().map(|i| encode_monty_object(env, i)).collect();
            encode_mapset(env, &members)
        }
        MontyObject::Path(p) => {
            let tag = Atom::from_str(env, "path").unwrap();
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), p.encode(env)])
        }
        MontyObject::NamedTuple {
            type_name,
            field_names,
            values,
        } => {
            let name_atom = Atom::from_str(env, &snake_case(type_name)).unwrap();
            let mut fields_map = rustler::types::map::map_new(env);
            for (fname, val) in field_names.iter().zip(values.iter()) {
                let key = Atom::from_str(env, fname).unwrap().encode(env);
                let value = encode_monty_object(env, val);
                fields_map = fields_map.map_put(key, value).unwrap();
            }
            rustler::types::tuple::make_tuple(env, &[name_atom.encode(env), fields_map])
        }
        MontyObject::Dataclass {
            name,
            field_names,
            attrs,
            frozen,
            ..
        } => {
            let struct_atom = Atom::from_str(env, "Elixir.ExMonty.Dataclass").unwrap();
            let mut fields_map = rustler::types::map::map_new(env);
            let attr_map: std::collections::HashMap<String, &MontyObject> = attrs
                .into_iter()
                .filter_map(|(k, v)| {
                    if let MontyObject::String(s) = k {
                        Some((s.clone(), v))
                    } else {
                        None
                    }
                })
                .collect();
            for fname in field_names {
                if let Some(val) = attr_map.get(fname) {
                    let key = Atom::from_str(env, fname).unwrap().encode(env);
                    let value = encode_monty_object(env, val);
                    fields_map = fields_map.map_put(key, value).unwrap();
                }
            }
            rustler::types::map::map_new(env)
                .map_put(
                    Atom::from_str(env, "__struct__").unwrap().encode(env),
                    struct_atom.encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "name").unwrap().encode(env),
                    name.encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "fields").unwrap().encode(env),
                    fields_map,
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "frozen").unwrap().encode(env),
                    frozen.encode(env),
                )
                .unwrap()
        }
        MontyObject::Exception { exc_type, arg } => {
            let struct_atom = Atom::from_str(env, "Elixir.ExMonty.Exception").unwrap();
            let type_str = exc_type.to_string();
            let type_atom = Atom::from_str(env, &snake_case(&type_str)).unwrap();
            let message = match arg {
                Some(msg) => msg.encode(env),
                None => rustler::types::atom::nil().encode(env),
            };
            rustler::types::map::map_new(env)
                .map_put(
                    Atom::from_str(env, "__struct__").unwrap().encode(env),
                    struct_atom.encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "type").unwrap().encode(env),
                    type_atom.encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "message").unwrap().encode(env),
                    message,
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "traceback").unwrap().encode(env),
                    Vec::<Term>::new().encode(env),
                )
                .unwrap()
        }
        MontyObject::Type(_ty) => {
            let repr = obj.to_string();
            Atom::from_str(env, &snake_case(&repr))
                .unwrap_or_else(|_| Atom::from_str(env, "unknown_type").unwrap())
                .encode(env)
        }
        MontyObject::BuiltinFunction(_) => {
            Atom::from_str(env, "builtin_function").unwrap().encode(env)
        }
        MontyObject::Repr(s) => {
            let tag = Atom::from_str(env, "repr").unwrap();
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), s.encode(env)])
        }
        MontyObject::Cycle(_, desc) => {
            let tag = Atom::from_str(env, "cycle").unwrap();
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), desc.encode(env)])
        }
    }
}

fn encode_mapset<'a>(env: Env<'a>, members: &[Term<'a>]) -> Term<'a> {
    let struct_atom = Atom::from_str(env, "Elixir.MapSet").unwrap();
    let mut inner_map = rustler::types::map::map_new(env);
    let placeholder: Vec<Term> = vec![];
    let placeholder_term = placeholder.encode(env);
    for member in members {
        inner_map = inner_map.map_put(*member, placeholder_term).unwrap();
    }
    rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "__struct__").unwrap().encode(env),
            struct_atom.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "map").unwrap().encode(env),
            inner_map,
        )
        .unwrap()
}

fn encode_bigint<'a>(env: Env<'a>, bi: &BigInt) -> Term<'a> {
    // Try to fit in i64 first
    if let Some(i) = bi.to_i64() {
        return i.encode(env);
    }
    // For larger values, convert through byte representation.
    // Erlang/BEAM natively supports big integers, so we convert via
    // the string representation and use Elixir's String.to_integer equivalent.
    // Since we can't call erlang:binary_to_integer from Rust NIF directly,
    // we'll use the two's complement byte representation.
    let (sign, bytes) = bi.to_bytes_be();
    let sign_int: i32 = match sign {
        num_bigint::Sign::Minus => -1,
        num_bigint::Sign::NoSign => 0,
        num_bigint::Sign::Plus => 1,
    };

    // Encode as {:__bigint__, sign, bytes} and let Elixir handle reconstruction
    let tag = Atom::from_str(env, "__bigint__").unwrap();
    let mut owned = rustler::OwnedBinary::new(bytes.len()).unwrap();
    owned.as_mut_slice().copy_from_slice(&bytes);
    let binary = owned.release(env);

    rustler::types::tuple::make_tuple(
        env,
        &[tag.encode(env), sign_int.encode(env), binary.encode(env)],
    )
}

// ── Decoding: Erlang Term → MontyObject ──────────────────────────────────────

pub fn decode_monty_object<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyObject> {
    // nil, true, false, ellipsis atoms
    if term.is_atom() {
        let atom_str: String = term.atom_to_string().map_err(|_| rustler::Error::BadArg)?;
        return match atom_str.as_str() {
            "nil" => Ok(MontyObject::None),
            "true" => Ok(MontyObject::Bool(true)),
            "false" => Ok(MontyObject::Bool(false)),
            "ellipsis" => Ok(MontyObject::Ellipsis),
            _ => Err(rustler::Error::BadArg),
        };
    }

    // Try i64 first (most common integer case)
    if let Ok(i) = term.decode::<i64>() {
        return Ok(MontyObject::Int(i));
    }

    // Float
    if term.is_float() {
        let f: f64 = term.decode()?;
        return Ok(MontyObject::Float(f));
    }

    // Binary/String
    if term.is_binary() {
        let s: String = term.decode()?;
        return Ok(MontyObject::String(s));
    }

    // Tuple - check for tagged tuples first
    if let Ok(elements) = get_tuple(term) {
        if elements.len() == 2 {
            if let Ok(tag) = elements[0].atom_to_string() {
                match tag.as_str() {
                    "bytes" => {
                        let binary: rustler::Binary = elements[1].decode()?;
                        return Ok(MontyObject::Bytes(binary.as_slice().to_vec()));
                    }
                    "path" => {
                        let path: String = elements[1].decode()?;
                        return Ok(MontyObject::Path(path));
                    }
                    "repr" => {
                        let repr: String = elements[1].decode()?;
                        return Ok(MontyObject::Repr(repr));
                    }
                    _ => {}
                }
            }
        }
        // Check for bigint tagged tuple {:__bigint__, sign, bytes}
        if elements.len() == 3 {
            if let Ok(tag) = elements[0].atom_to_string() {
                if tag == "__bigint__" {
                    let sign: i32 = elements[1].decode()?;
                    let binary: rustler::Binary = elements[2].decode()?;
                    let num_sign = match sign {
                        -1 => num_bigint::Sign::Minus,
                        0 => num_bigint::Sign::NoSign,
                        _ => num_bigint::Sign::Plus,
                    };
                    let bi = BigInt::from_bytes_be(num_sign, binary.as_slice());
                    return Ok(MontyObject::BigInt(bi));
                }
            }
        }
        // Check for NamedTuple format: {atom, %{field_atom => value}}
        if elements.len() == 2 {
            if let Ok(type_name) = elements[0].atom_to_string() {
                if elements[1].is_map() {
                    if let Some(iter) = MapIterator::new(elements[1]) {
                        let mut field_names = Vec::new();
                        let mut values = Vec::new();
                        let mut all_atom_keys = true;
                        for (k, v) in iter {
                            if let Ok(key_name) = k.atom_to_string() {
                                field_names.push(key_name);
                                match decode_monty_object(env, v) {
                                    Ok(val) => values.push(val),
                                    Err(_) => { all_atom_keys = false; break; }
                                }
                            } else {
                                all_atom_keys = false;
                                break;
                            }
                        }
                        if all_atom_keys && !field_names.is_empty() {
                            return Ok(MontyObject::NamedTuple {
                                type_name: pascal_case(&type_name),
                                field_names,
                                values,
                            });
                        }
                    }
                }
            }
        }
        // Regular tuple
        let items: Vec<MontyObject> = elements
            .iter()
            .map(|t| decode_monty_object(env, *t))
            .collect::<NifResult<Vec<_>>>()?;
        return Ok(MontyObject::Tuple(items));
    }

    // List
    if term.is_list() {
        let list: Vec<Term> = term.decode()?;
        let items: Vec<MontyObject> = list
            .into_iter()
            .map(|t| decode_monty_object(env, t))
            .collect::<NifResult<Vec<_>>>()?;
        return Ok(MontyObject::List(items));
    }

    // Map - check for MapSet struct
    if term.is_map() {
        let struct_key = Atom::from_str(env, "__struct__").unwrap().encode(env);
        if let Ok(struct_val) = term.map_get(struct_key) {
            if let Ok(struct_name) = struct_val.atom_to_string() {
                if struct_name == "Elixir.MapSet" {
                    let map_key = Atom::from_str(env, "map").unwrap().encode(env);
                    let inner_map = term.map_get(map_key).map_err(|_| rustler::Error::BadArg)?;
                    let iter =
                        MapIterator::new(inner_map).ok_or(rustler::Error::BadArg)?;
                    let items: Vec<MontyObject> = iter
                        .map(|(k, _v)| decode_monty_object(env, k))
                        .collect::<NifResult<Vec<_>>>()?;
                    return Ok(MontyObject::Set(items));
                }
            }
        }
        // Regular map → Dict
        let iter = MapIterator::new(term).ok_or(rustler::Error::BadArg)?;
        let pairs: Vec<(MontyObject, MontyObject)> = iter
            .map(|(k, v)| {
                let key = decode_monty_object(env, k)?;
                let val = decode_monty_object(env, v)?;
                Ok((key, val))
            })
            .collect::<NifResult<Vec<_>>>()?;
        return Ok(MontyObject::dict(pairs));
    }

    // If we got here and the term is a number but didn't decode as i64,
    // it could be a big integer. Try to handle via is_number check.
    // Unfortunately Rustler doesn't provide a direct bigint decode,
    // so big integers that don't fit i64 need to go through Elixir-side conversion.
    // For now, return an error for unsupported types.
    Err(rustler::Error::BadArg)
}

// ── Helper: Decode named inputs ──────────────────────────────────────────────

pub fn decode_inputs<'a>(
    env: Env<'a>,
    inputs: Vec<(String, Term<'a>)>,
) -> NifResult<Vec<MontyObject>> {
    inputs
        .into_iter()
        .map(|(_name, term)| decode_monty_object(env, term))
        .collect()
}

// ── Helper: Decode ResourceLimits from Elixir map ────────────────────────────

pub fn decode_resource_limits(term: Term) -> NifResult<ResourceLimits> {
    if term.is_atom() {
        let s = term.atom_to_string().map_err(|_| rustler::Error::BadArg)?;
        if s == "nil" {
            return Ok(ResourceLimits::new());
        }
    }

    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }

    let mut limits = ResourceLimits::new();
    let env = term.get_env();

    if let Ok(val) = term.map_get(Atom::from_str(env, "max_allocations").unwrap().encode(env)) {
        if let Ok(n) = val.decode::<usize>() {
            limits = limits.max_allocations(n);
        }
    }

    if let Ok(val) = term.map_get(Atom::from_str(env, "max_duration_secs").unwrap().encode(env)) {
        if let Ok(secs) = val.decode::<f64>() {
            limits = limits.max_duration(Duration::from_secs_f64(secs));
        }
    }

    if let Ok(val) = term.map_get(Atom::from_str(env, "max_memory").unwrap().encode(env)) {
        if let Ok(n) = val.decode::<usize>() {
            limits = limits.max_memory(n);
        }
    }

    if let Ok(val) = term.map_get(Atom::from_str(env, "gc_interval").unwrap().encode(env)) {
        if let Ok(n) = val.decode::<usize>() {
            limits = limits.gc_interval(n);
        }
    }

    if let Ok(val) = term.map_get(Atom::from_str(env, "max_recursion_depth").unwrap().encode(env))
    {
        if let Ok(n) = val.decode::<usize>() {
            limits = limits.max_recursion_depth(Some(n));
        }
    }

    Ok(limits)
}

pub fn encode_os_function<'a>(env: Env<'a>, func: &OsFunction) -> Term<'a> {
    let name = match func {
        OsFunction::Exists => "exists",
        OsFunction::IsFile => "is_file",
        OsFunction::IsDir => "is_dir",
        OsFunction::IsSymlink => "is_symlink",
        OsFunction::ReadText => "read_text",
        OsFunction::ReadBytes => "read_bytes",
        OsFunction::WriteText => "write_text",
        OsFunction::WriteBytes => "write_bytes",
        OsFunction::Mkdir => "mkdir",
        OsFunction::Unlink => "unlink",
        OsFunction::Rmdir => "rmdir",
        OsFunction::Iterdir => "iterdir",
        OsFunction::Stat => "stat",
        OsFunction::Rename => "rename",
        OsFunction::Resolve => "resolve",
        OsFunction::Absolute => "absolute",
        OsFunction::Getenv => "getenv",
        OsFunction::GetEnviron => "get_environ",
    };
    Atom::from_str(env, name).unwrap().encode(env)
}

fn pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + &chars.collect::<String>()
                }
                None => String::new(),
            }
        })
        .collect()
}

fn snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}
