use monty::{MontyObject, OsFunction, ResourceLimits};
use num_bigint::BigInt;
use rustler::types::atom::Atom;
use rustler::types::map::MapIterator;
use rustler::types::tuple::get_tuple;
use rustler::{Encoder, Env, NifResult, Term};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

// ── Encoding: MontyObject → Erlang Term ──────────────────────────────────────

pub fn encode_monty_object<'a>(env: Env<'a>, obj: &MontyObject) -> Term<'a> {
    match obj {
        MontyObject::None => rustler::types::atom::nil().encode(env),
        MontyObject::Bool(b) => b.encode(env),
        MontyObject::Int(i) => i.encode(env),
        MontyObject::BigInt(bi) => bi.encode(env),
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
            let tag = Atom::from_str(env, "named_tuple").unwrap();

            let fields: Vec<Term> = field_names
                .iter()
                .zip(values.iter())
                .map(|(fname, val)| {
                    let key = fname.encode(env);
                    let value = encode_monty_object(env, val);
                    rustler::types::tuple::make_tuple(env, &[key, value])
                })
                .collect();

            rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), type_name.encode(env), fields.encode(env)],
            )
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
                    let key = fname.encode(env);
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
                .map_put(Atom::from_str(env, "message").unwrap().encode(env), message)
                .unwrap()
                .map_put(
                    Atom::from_str(env, "traceback").unwrap().encode(env),
                    Vec::<Term>::new().encode(env),
                )
                .unwrap()
        }
        MontyObject::Type(ty) => {
            let repr = ty.to_string();
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
        .map_put(Atom::from_str(env, "map").unwrap().encode(env), inner_map)
        .unwrap()
}

const STAT_RESULT_FIELD_ORDER: [&str; 10] = [
    "st_mode", "st_ino", "st_dev", "st_nlink", "st_uid", "st_gid", "st_size", "st_atime",
    "st_mtime", "st_ctime",
];

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
            other => Ok(MontyObject::String(other.to_owned())),
        };
    }

    // Try i64 first (most common integer case)
    if let Ok(i) = term.decode::<i64>() {
        return Ok(MontyObject::Int(i));
    }

    // Big integer (arbitrary precision)
    if let Ok(bi) = term.decode::<BigInt>() {
        return Ok(MontyObject::BigInt(bi));
    }

    // Float
    if term.is_float() {
        let f: f64 = term.decode()?;
        return Ok(MontyObject::Float(f));
    }

    // Binary/String
    if term.is_binary() {
        if let Ok(s) = term.decode::<String>() {
            return Ok(MontyObject::String(s));
        }

        let binary: rustler::Binary = term.decode()?;
        return Ok(MontyObject::Bytes(binary.as_slice().to_vec()));
    }

    // Tuple - check for tagged tuples first
    if let Ok(elements) = get_tuple(term) {
        // Tagged NamedTuple: {:named_tuple, type_name, fields}
        if elements.len() == 3 {
            if let Ok(tag) = elements[0].atom_to_string() {
                if tag == "named_tuple" {
                    return decode_named_tuple(env, elements[1], elements[2]);
                }
            }
        }

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
                    let iter = MapIterator::new(inner_map).ok_or(rustler::Error::BadArg)?;
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

    Err(rustler::Error::BadArg)
}

// ── Helper: Decode named inputs ──────────────────────────────────────────────

pub fn decode_inputs<'a>(
    env: Env<'a>,
    inputs: Vec<(String, Term<'a>)>,
    expected_input_names: &[String],
) -> NifResult<Vec<MontyObject>> {
    if expected_input_names.is_empty() {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }

        return Err(rustler::Error::Term(Box::new(format!(
            "unexpected inputs: expected none, got {}",
            inputs.len()
        ))));
    }

    let mut expected_set: HashSet<&str> = HashSet::with_capacity(expected_input_names.len());
    for name in expected_input_names {
        if !expected_set.insert(name.as_str()) {
            return Err(rustler::Error::Term(Box::new(format!(
                "runner has duplicate input name: {name}"
            ))));
        }
    }

    let mut provided: HashMap<String, MontyObject> = HashMap::with_capacity(inputs.len());
    for (name, term) in inputs {
        if provided.contains_key(&name) {
            return Err(rustler::Error::Term(Box::new(format!(
                "duplicate input provided: {name}"
            ))));
        }

        let value = decode_monty_object(env, term)?;
        provided.insert(name, value);
    }

    let mut missing: BTreeSet<&str> = BTreeSet::new();
    let mut ordered: Vec<MontyObject> = Vec::with_capacity(expected_input_names.len());
    for name in expected_input_names {
        match provided.remove(name) {
            Some(val) => ordered.push(val),
            None => {
                missing.insert(name);
            }
        }
    }

    if !missing.is_empty() {
        let missing_list = missing.into_iter().collect::<Vec<_>>().join(", ");
        return Err(rustler::Error::Term(Box::new(format!(
            "missing required inputs: {missing_list}"
        ))));
    }

    if !provided.is_empty() {
        let mut unexpected = provided.keys().cloned().collect::<Vec<_>>();
        unexpected.sort();
        return Err(rustler::Error::Term(Box::new(format!(
            "unexpected inputs: {}",
            unexpected.join(", ")
        ))));
    }

    Ok(ordered)
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

    if let Ok(val) = term.map_get(
        Atom::from_str(env, "max_duration_secs")
            .unwrap()
            .encode(env),
    ) {
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

    if let Ok(val) = term.map_get(
        Atom::from_str(env, "max_recursion_depth")
            .unwrap()
            .encode(env),
    ) {
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

fn decode_named_tuple<'a>(
    env: Env<'a>,
    type_term: Term<'a>,
    fields_term: Term<'a>,
) -> NifResult<MontyObject> {
    let raw_type_name: String = if type_term.is_atom() {
        type_term
            .atom_to_string()
            .map_err(|_| rustler::Error::BadArg)?
    } else if type_term.is_binary() {
        type_term.decode()?
    } else {
        return Err(rustler::Error::BadArg);
    };

    let type_name = normalize_namedtuple_type_name(&raw_type_name);

    // Prefer order-preserving list-of-pairs representation.
    if fields_term.is_list() {
        let fields: Vec<Term> = fields_term.decode()?;
        let mut field_names = Vec::with_capacity(fields.len());
        let mut values = Vec::with_capacity(fields.len());

        for item in fields {
            let elems = get_tuple(item).map_err(|_| rustler::Error::BadArg)?;
            if elems.len() != 2 {
                return Err(rustler::Error::BadArg);
            }

            let field_name: String = if elems[0].is_atom() {
                elems[0]
                    .atom_to_string()
                    .map_err(|_| rustler::Error::BadArg)?
            } else if elems[0].is_binary() {
                elems[0].decode()?
            } else {
                return Err(rustler::Error::BadArg);
            };

            let value = decode_monty_object(env, elems[1])?;
            field_names.push(field_name);
            values.push(value);
        }

        return Ok(MontyObject::NamedTuple {
            type_name,
            field_names,
            values,
        });
    }

    if fields_term.is_map() {
        let iter = MapIterator::new(fields_term).ok_or(rustler::Error::BadArg)?;

        let mut by_name: HashMap<String, MontyObject> = HashMap::new();
        for (k, v) in iter {
            let field_name: String = if k.is_atom() {
                k.atom_to_string().map_err(|_| rustler::Error::BadArg)?
            } else if k.is_binary() {
                k.decode()?
            } else {
                return Err(rustler::Error::BadArg);
            };

            let value = decode_monty_object(env, v)?;
            by_name.insert(field_name, value);
        }

        let (field_names, values) = order_named_tuple_fields(&type_name, by_name)?;

        return Ok(MontyObject::NamedTuple {
            type_name,
            field_names,
            values,
        });
    }

    Err(rustler::Error::BadArg)
}

fn normalize_namedtuple_type_name(s: &str) -> String {
    // Monty uses PascalCase type names for built-in named tuples (e.g. "StatResult").
    // Allow snake_case inputs for convenience.
    if s.chars().any(|c| c.is_uppercase()) {
        s.to_owned()
    } else {
        pascal_case(s)
    }
}

fn order_named_tuple_fields(
    type_name: &str,
    mut by_name: HashMap<String, MontyObject>,
) -> NifResult<(Vec<String>, Vec<MontyObject>)> {
    if type_name == "StatResult" {
        let mut field_names = Vec::with_capacity(STAT_RESULT_FIELD_ORDER.len());
        let mut values = Vec::with_capacity(STAT_RESULT_FIELD_ORDER.len());

        for name in STAT_RESULT_FIELD_ORDER {
            let val = by_name.remove(name).ok_or(rustler::Error::BadArg)?;
            field_names.push(name.to_owned());
            values.push(val);
        }

        if !by_name.is_empty() {
            return Err(rustler::Error::BadArg);
        }

        return Ok((field_names, values));
    }

    let mut field_names = by_name.keys().cloned().collect::<Vec<_>>();
    field_names.sort();
    let values = field_names
        .iter()
        .map(|name| by_name.remove(name).unwrap())
        .collect();
    Ok((field_names, values))
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
