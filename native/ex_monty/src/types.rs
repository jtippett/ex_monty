use monty_types::{
    DictPairs, FileMode, MontyDate, MontyDateTime, MontyFileHandle, MontyObject, MontyTimeDelta,
    MontyTimeZone, OsFunctionCall, ResourceLimits,
};
use num_bigint::BigInt;
use rustler::types::atom::Atom;
use rustler::types::map::MapIterator;
use rustler::types::tuple::get_tuple;
use rustler::{Encoder, Env, NifResult, Term};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

/// Maximum nesting depth when converting `MontyObject` <-> Erlang term.
///
/// `encode_monty_object`/`decode_monty_object` recurse for every nested
/// container, and the resulting nested `MontyObject` tree is also *dropped*
/// recursively. A deeply-nested value — which untrusted Monty code can build
/// iteratively (`x = []; for _ in range(n): x = [x]`) well past Monty's own
/// recursion limit — would overflow the (small) dirty-scheduler native stack
/// during traversal or drop, and a stack overflow inside a NIF aborts the
/// **entire BEAM** (it is NOT catchable by Rustler's panic boundary). So we
/// reject anything deeper than this with a clean error before such a tree is
/// ever built. 64 is far beyond any legitimate data nesting yet leaves a large
/// safety margin below the empirically observed overflow point (~150+ frames),
/// including headroom for the unguarded recursive `Drop` and for smaller
/// scheduler stacks on other platforms.
const MAX_NESTING_DEPTH: usize = 64;

/// Maximum magnitude length (bytes) accepted for a `{:__bigint__, sign, bytes}`
/// tagged tuple. 1 KiB is ~2466 decimal digits — beyond any legitimate integer
/// — and caps allocation from a malicious callback payload.
const MAX_BIGINT_BYTES: usize = 1024;

fn nesting_error() -> rustler::Error {
    rustler::Error::Term(Box::new("value nesting exceeds ExMonty MAX_NESTING_DEPTH"))
}

// ── Encoding: MontyObject → Erlang Term ──────────────────────────────────────

pub fn encode_monty_object<'a>(env: Env<'a>, obj: &MontyObject) -> NifResult<Term<'a>> {
    encode_monty_object_depth(env, obj, 0)
}

fn encode_monty_object_depth<'a>(
    env: Env<'a>,
    obj: &MontyObject,
    depth: usize,
) -> NifResult<Term<'a>> {
    if depth > MAX_NESTING_DEPTH {
        return Err(nesting_error());
    }
    let term = match obj {
        MontyObject::None => rustler::types::atom::nil().encode(env),
        MontyObject::NotImplemented => Atom::from_str(env, "not_implemented").unwrap().encode(env),
        MontyObject::Bool(b) => b.encode(env),
        MontyObject::Int(i) => i.encode(env),
        MontyObject::BigInt(bi) => bi.encode(env),
        MontyObject::Float(f) => {
            if f.is_infinite() {
                if f.is_sign_positive() {
                    Atom::from_str(env, "infinity").unwrap().encode(env)
                } else {
                    Atom::from_str(env, "neg_infinity").unwrap().encode(env)
                }
            } else if f.is_nan() {
                Atom::from_str(env, "nan").unwrap().encode(env)
            } else {
                f.encode(env)
            }
        }
        MontyObject::String(s) => s.encode(env),
        MontyObject::Bytes(b) => {
            let tag = Atom::from_str(env, "bytes").unwrap();
            let mut owned = rustler::OwnedBinary::new(b.len()).ok_or_else(|| {
                rustler::Error::Term(Box::new("failed to allocate binary for bytes value"))
            })?;
            owned.as_mut_slice().copy_from_slice(b);
            let binary = owned.release(env);
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), binary.encode(env)])
        }
        MontyObject::Ellipsis => Atom::from_str(env, "ellipsis").unwrap().encode(env),
        MontyObject::List(items) => {
            let terms: Vec<Term> = items
                .iter()
                .map(|i| encode_monty_object_depth(env, i, depth + 1))
                .collect::<NifResult<Vec<_>>>()?;
            terms.encode(env)
        }
        MontyObject::Tuple(items) => {
            let terms: Vec<Term> = items
                .iter()
                .map(|i| encode_monty_object_depth(env, i, depth + 1))
                .collect::<NifResult<Vec<_>>>()?;
            rustler::types::tuple::make_tuple(env, &terms)
        }
        MontyObject::Dict(pairs) => {
            let mut map = rustler::types::map::map_new(env);
            for (k, v) in pairs {
                let key = encode_monty_object_depth(env, k, depth + 1)?;
                let val = encode_monty_object_depth(env, v, depth + 1)?;
                map = map.map_put(key, val).unwrap();
            }
            map
        }
        MontyObject::Set(items) | MontyObject::FrozenSet(items) => {
            let members: Vec<Term> = items
                .iter()
                .map(|i| encode_monty_object_depth(env, i, depth + 1))
                .collect::<NifResult<Vec<_>>>()?;
            encode_mapset(env, &members)
        }
        MontyObject::Path(p) => {
            let tag = Atom::from_str(env, "path").unwrap();
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), p.encode(env)])
        }
        MontyObject::FileHandle(handle) => {
            let tag = Atom::from_str(env, "file_handle").unwrap();
            let map = rustler::types::map::map_new(env)
                .map_put(
                    Atom::from_str(env, "path").unwrap().encode(env),
                    handle.path.encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "mode").unwrap().encode(env),
                    handle.mode.as_str().encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "position").unwrap().encode(env),
                    handle.position.encode(env),
                )
                .unwrap();
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), map])
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
                    let value = encode_monty_object_depth(env, val, depth + 1)?;
                    Ok(rustler::types::tuple::make_tuple(env, &[key, value]))
                })
                .collect::<NifResult<Vec<_>>>()?;

            rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), type_name.encode(env), fields.encode(env)],
            )
        }
        MontyObject::Dataclass {
            name,
            type_id,
            field_names,
            attrs,
            frozen,
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
                    let value = encode_monty_object_depth(env, val, depth + 1)?;
                    fields_map = fields_map.map_put(key, value).unwrap();
                }
            }
            let field_names_term: Vec<Term> = field_names.iter().map(|s| s.encode(env)).collect();
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
                    Atom::from_str(env, "field_names").unwrap().encode(env),
                    field_names_term.encode(env),
                )
                .unwrap()
                .map_put(
                    Atom::from_str(env, "type_id").unwrap().encode(env),
                    type_id.encode(env),
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
        MontyObject::Function { name, docstring } => {
            let tag = Atom::from_str(env, "function").unwrap();
            let doc = match docstring {
                Some(d) => d.encode(env),
                None => rustler::types::atom::nil().encode(env),
            };
            rustler::types::tuple::make_tuple(env, &[tag.encode(env), name.encode(env), doc])
        }
        MontyObject::Date(d) => encode_date(env, d),
        MontyObject::DateTime(dt) => encode_datetime(env, dt),
        MontyObject::TimeDelta(td) => encode_timedelta(env, td),
        MontyObject::TimeZone(tz) => encode_timezone(env, tz),
    };
    Ok(term)
}

fn encode_date<'a>(env: Env<'a>, d: &MontyDate) -> Term<'a> {
    let tag = Atom::from_str(env, "date").unwrap();
    let map = rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "year").unwrap().encode(env),
            d.year.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "month").unwrap().encode(env),
            d.month.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "day").unwrap().encode(env),
            d.day.encode(env),
        )
        .unwrap();
    rustler::types::tuple::make_tuple(env, &[tag.encode(env), map])
}

fn encode_datetime<'a>(env: Env<'a>, dt: &MontyDateTime) -> Term<'a> {
    let tag = Atom::from_str(env, "datetime").unwrap();
    let nil = rustler::types::atom::nil().encode(env);
    let offset_term = match dt.offset_seconds {
        Some(s) => s.encode(env),
        None => nil,
    };
    let tz_name_term = match &dt.timezone_name {
        Some(s) => s.encode(env),
        None => nil,
    };
    let map = rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "year").unwrap().encode(env),
            dt.year.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "month").unwrap().encode(env),
            dt.month.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "day").unwrap().encode(env),
            dt.day.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "hour").unwrap().encode(env),
            dt.hour.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "minute").unwrap().encode(env),
            dt.minute.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "second").unwrap().encode(env),
            dt.second.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "microsecond").unwrap().encode(env),
            dt.microsecond.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "offset_seconds").unwrap().encode(env),
            offset_term,
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "tz_name").unwrap().encode(env),
            tz_name_term,
        )
        .unwrap();
    rustler::types::tuple::make_tuple(env, &[tag.encode(env), map])
}

fn encode_timedelta<'a>(env: Env<'a>, td: &MontyTimeDelta) -> Term<'a> {
    let tag = Atom::from_str(env, "timedelta").unwrap();
    let map = rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "days").unwrap().encode(env),
            td.days.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "seconds").unwrap().encode(env),
            td.seconds.encode(env),
        )
        .unwrap()
        .map_put(
            Atom::from_str(env, "microseconds").unwrap().encode(env),
            td.microseconds.encode(env),
        )
        .unwrap();
    rustler::types::tuple::make_tuple(env, &[tag.encode(env), map])
}

fn encode_timezone<'a>(env: Env<'a>, tz: &MontyTimeZone) -> Term<'a> {
    let tag = Atom::from_str(env, "timezone").unwrap();
    let nil = rustler::types::atom::nil().encode(env);
    let name_term = match &tz.name {
        Some(s) => s.encode(env),
        None => nil,
    };
    let map = rustler::types::map::map_new(env)
        .map_put(
            Atom::from_str(env, "offset_seconds").unwrap().encode(env),
            tz.offset_seconds.encode(env),
        )
        .unwrap()
        .map_put(Atom::from_str(env, "name").unwrap().encode(env), name_term)
        .unwrap();
    rustler::types::tuple::make_tuple(env, &[tag.encode(env), map])
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
    decode_monty_object_depth(env, term, 0)
}

fn decode_monty_object_depth<'a>(
    env: Env<'a>,
    term: Term<'a>,
    depth: usize,
) -> NifResult<MontyObject> {
    if depth > MAX_NESTING_DEPTH {
        return Err(nesting_error());
    }
    // nil, true, false, ellipsis atoms
    if term.is_atom() {
        let atom_str: String = term.atom_to_string().map_err(|_| rustler::Error::BadArg)?;
        return match atom_str.as_str() {
            "nil" => Ok(MontyObject::None),
            "true" => Ok(MontyObject::Bool(true)),
            "false" => Ok(MontyObject::Bool(false)),
            "ellipsis" => Ok(MontyObject::Ellipsis),
            "not_implemented" => Ok(MontyObject::NotImplemented),
            "infinity" => Ok(MontyObject::Float(f64::INFINITY)),
            "neg_infinity" => Ok(MontyObject::Float(f64::NEG_INFINITY)),
            "nan" => Ok(MontyObject::Float(f64::NAN)),
            other => Ok(MontyObject::String(other.to_owned())),
        };
    }

    // Try i64 first (most common integer case)
    if let Ok(i) = term.decode::<i64>() {
        return Ok(MontyObject::Int(i));
    }

    // Big integer (arbitrary precision)
    if let Ok(bi) = term.decode::<BigInt>() {
        if bi.bits() > (MAX_BIGINT_BYTES as u64) * 8 {
            return Err(bigint_size_error());
        }
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
                    return decode_named_tuple(env, elements[1], elements[2], depth);
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
                    "file_handle" => {
                        return Ok(MontyObject::FileHandle(decode_file_handle(
                            env,
                            elements[1],
                        )?));
                    }
                    "repr" => {
                        let repr: String = elements[1].decode()?;
                        return Ok(MontyObject::Repr(repr));
                    }
                    "function" => {
                        let name: String = elements[1].decode()?;
                        return Ok(MontyObject::Function {
                            name,
                            docstring: None,
                        });
                    }
                    "date" => {
                        return Ok(MontyObject::Date(decode_date_fields(env, elements[1])?));
                    }
                    "datetime" => {
                        return Ok(MontyObject::DateTime(decode_datetime_fields(
                            env,
                            elements[1],
                        )?));
                    }
                    "timedelta" => {
                        return Ok(MontyObject::TimeDelta(decode_timedelta_fields(
                            env,
                            elements[1],
                        )?));
                    }
                    "timezone" => {
                        return Ok(MontyObject::TimeZone(decode_timezone_fields(
                            env,
                            elements[1],
                        )?));
                    }
                    _ => {}
                }
            }
        }
        // {:function, name, docstring}
        if elements.len() == 3 {
            if let Ok(tag) = elements[0].atom_to_string() {
                if tag == "function" {
                    let name: String = elements[1].decode()?;
                    let docstring: Option<String> = if elements[2].is_atom() {
                        let s = elements[2]
                            .atom_to_string()
                            .map_err(|_| rustler::Error::BadArg)?;
                        if s == "nil" {
                            None
                        } else {
                            Some(s)
                        }
                    } else {
                        Some(elements[2].decode()?)
                    };
                    return Ok(MontyObject::Function { name, docstring });
                }
            }
        }
        // Check for bigint tagged tuple {:__bigint__, sign, bytes}
        if elements.len() == 3 {
            if let Ok(tag) = elements[0].atom_to_string() {
                if tag == "__bigint__" {
                    let sign: i32 = elements[1].decode()?;
                    let binary: rustler::Binary = elements[2].decode()?;
                    let bytes = binary.as_slice();
                    if bytes.len() > MAX_BIGINT_BYTES {
                        return Err(bigint_size_error());
                    }
                    let num_sign = match sign {
                        -1 => num_bigint::Sign::Minus,
                        0 => num_bigint::Sign::NoSign,
                        1 => num_bigint::Sign::Plus,
                        _ => {
                            return Err(rustler::Error::Term(Box::new(
                                "bigint sign must be -1, 0, or 1",
                            )))
                        }
                    };
                    // Enforce canonical form: sign 0 iff magnitude is zero. This
                    // keeps the only foreign-controlled integer encoding total and
                    // unambiguous instead of silently coercing junk.
                    let magnitude_is_zero = bytes.iter().all(|&b| b == 0);
                    if (num_sign == num_bigint::Sign::NoSign) != magnitude_is_zero {
                        return Err(rustler::Error::Term(Box::new(
                            "bigint sign 0 must have zero magnitude and vice versa",
                        )));
                    }
                    let bi = BigInt::from_bytes_be(num_sign, bytes);
                    return Ok(MontyObject::BigInt(bi));
                }
            }
        }
        // Regular tuple
        let items: Vec<MontyObject> = elements
            .iter()
            .map(|t| decode_monty_object_depth(env, *t, depth + 1))
            .collect::<NifResult<Vec<_>>>()?;
        return Ok(MontyObject::Tuple(items));
    }

    // List
    if term.is_list() {
        let list: Vec<Term> = term.decode()?;
        let items: Vec<MontyObject> = list
            .into_iter()
            .map(|t| decode_monty_object_depth(env, t, depth + 1))
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
                        .map(|(k, _v)| decode_monty_object_depth(env, k, depth + 1))
                        .collect::<NifResult<Vec<_>>>()?;
                    return Ok(MontyObject::Set(items));
                }
                if struct_name == "Elixir.ExMonty.Dataclass" {
                    return decode_dataclass(env, term, depth);
                }
            }
        }
        // Regular map → Dict
        let iter = MapIterator::new(term).ok_or(rustler::Error::BadArg)?;
        let pairs: Vec<(MontyObject, MontyObject)> = iter
            .map(|(k, v)| {
                let key = decode_monty_object_depth(env, k, depth + 1)?;
                let val = decode_monty_object_depth(env, v, depth + 1)?;
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

    if inputs.len() > expected_input_names.len() {
        return Err(rustler::Error::Term(Box::new(format!(
            "too many inputs: expected {}, got {}",
            expected_input_names.len(),
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
        if !expected_set.contains(name.as_str()) {
            return Err(rustler::Error::Term(Box::new(format!(
                "unexpected input: {name}"
            ))));
        }

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
        if matches!(s.as_str(), "nil" | "unlimited") {
            return Ok(cap_recursion_depth(ResourceLimits::default()));
        }
    }

    if !term.is_map() {
        // Use a descriptive Term error (not BadArg) so it surfaces through the
        // Elixir wrappers as a clean `{:error, _}` rather than an ArgumentError.
        return Err(rustler::Error::Term(Box::new(
            "limits must be a map, nil, or :unlimited",
        )));
    }

    let mut limits = ResourceLimits::default();
    let env = term.get_env();

    // A *present* limit key with a malformed value is rejected, not silently
    // dropped: silently ignoring it would degrade the limit to "unlimited" and
    // quietly weaken the sandbox. Absent keys keep their `ResourceLimits`
    // default.
    //
    // monty v0.0.21 removed allocation counting entirely (memory is bounded
    // via `max_memory` instead). Accepting-and-ignoring `max_allocations`
    // would pretend a limit is enforced when it isn't, so its presence is an
    // error.
    if term
        .map_get(Atom::from_str(env, "max_allocations").unwrap().encode(env))
        .is_ok()
    {
        return Err(rustler::Error::Term(Box::new(
            "max_allocations is no longer supported (removed in monty v0.0.21); use max_memory",
        )));
    }

    if let Ok(val) = term.map_get(
        Atom::from_str(env, "max_duration_secs")
            .unwrap()
            .encode(env),
    ) {
        let secs: f64 = val
            .decode()
            .map_err(|_| invalid_limit("max_duration_secs"))?;
        // `Duration::from_secs_f64` panics on negative / NaN / non-finite /
        // overflowing input; the fallible form turns that into a clean error
        // instead of a (caught) NIF panic.
        let duration =
            Duration::try_from_secs_f64(secs).map_err(|_| invalid_limit("max_duration_secs"))?;
        limits = limits.max_duration(duration);
    }

    if let Ok(val) = term.map_get(Atom::from_str(env, "max_memory").unwrap().encode(env)) {
        let n: usize = val.decode().map_err(|_| invalid_limit("max_memory"))?;
        limits = limits.max_memory(n);
    }

    if let Ok(val) = term.map_get(Atom::from_str(env, "gc_interval").unwrap().encode(env)) {
        let n: usize = val.decode().map_err(|_| invalid_limit("gc_interval"))?;
        limits = limits.gc_interval(n);
    }

    if let Ok(val) = term.map_get(
        Atom::from_str(env, "max_recursion_depth")
            .unwrap()
            .encode(env),
    ) {
        let n: usize = val
            .decode()
            .map_err(|_| invalid_limit("max_recursion_depth"))?;
        limits = limits.max_recursion_depth(n);
    }

    Ok(cap_recursion_depth(limits))
}

/// Hard upper bound on the recursion depth Monty is allowed, enforced here in
/// the NIF so it cannot be bypassed by calling `ExMonty.Native.*` directly or
/// by passing `nil`/a huge value. Monty converts a returned value to its output
/// representation recursively and that nested `MontyObject` is dropped
/// recursively on a small dirty-scheduler stack; allowing Monty to build one
/// deeper than a few hundred levels would overflow that stack and abort the
/// whole BEAM. Kept well below the empirically observed ~325-level overflow,
/// with margin for smaller scheduler stacks on other platforms.
const SAFE_MAX_RECURSION_DEPTH: usize = 128;

fn cap_recursion_depth(mut limits: ResourceLimits) -> ResourceLimits {
    // Upstream's `max_recursion_depth` is now always-bounded (plain usize),
    // but its default (1000) is still far past what the dirty-scheduler stack
    // tolerates, so the NIF-level cap stays.
    limits.max_recursion_depth = limits.max_recursion_depth.min(SAFE_MAX_RECURSION_DEPTH);
    limits
}

fn invalid_limit(key: &str) -> rustler::Error {
    rustler::Error::Term(Box::new(format!("invalid resource limit value for {key}")))
}

fn invalid_dataclass(msg: &'static str) -> rustler::Error {
    rustler::Error::Term(Box::new(format!("invalid dataclass: {msg}")))
}

/// True only for the literal `nil` atom (an explicitly-unset optional field),
/// not for arbitrary atoms — so a malformed atom value is rejected rather than
/// silently treated as "unset".
fn is_nil_atom(term: Term) -> bool {
    term.is_atom() && matches!(term.atom_to_string().as_deref(), Ok("nil"))
}

pub fn encode_os_function<'a>(env: Env<'a>, func: &OsFunctionCall) -> Term<'a> {
    let name = match func {
        OsFunctionCall::Exists(_) => "exists",
        OsFunctionCall::IsFile(_) => "is_file",
        OsFunctionCall::IsDir(_) => "is_dir",
        OsFunctionCall::IsSymlink(_) => "is_symlink",
        OsFunctionCall::ReadText(_) => "read_text",
        OsFunctionCall::ReadBytes(_) => "read_bytes",
        OsFunctionCall::WriteText(_) => "write_text",
        OsFunctionCall::WriteBytes(_) => "write_bytes",
        OsFunctionCall::AppendText(_) => "append_text",
        OsFunctionCall::AppendBytes(_) => "append_bytes",
        OsFunctionCall::Open(_) => "open",
        OsFunctionCall::Mkdir(_) => "mkdir",
        OsFunctionCall::Unlink(_) => "unlink",
        OsFunctionCall::Rmdir(_) => "rmdir",
        OsFunctionCall::Iterdir(_) => "iterdir",
        OsFunctionCall::Stat(_) => "stat",
        OsFunctionCall::Rename(_) => "rename",
        OsFunctionCall::Resolve(_) => "resolve",
        OsFunctionCall::Absolute(_) => "absolute",
        OsFunctionCall::Getenv(_) => "getenv",
        OsFunctionCall::GetEnviron => "get_environ",
        OsFunctionCall::DateToday => "date_today",
        OsFunctionCall::DateTimeNow(_) => "datetime_now",
    };
    Atom::from_str(env, name).unwrap().encode(env)
}

fn map_get_atom<'a>(env: Env<'a>, map: Term<'a>, key: &str) -> NifResult<Term<'a>> {
    map.map_get(Atom::from_str(env, key).unwrap().encode(env))
        .map_err(|_| rustler::Error::BadArg)
}

fn map_get_optional_string<'a>(
    env: Env<'a>,
    map: Term<'a>,
    key: &str,
) -> NifResult<Option<String>> {
    let term = match map.map_get(Atom::from_str(env, key).unwrap().encode(env)) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    if term.is_atom() {
        let s = term.atom_to_string().map_err(|_| rustler::Error::BadArg)?;
        if s == "nil" {
            return Ok(None);
        }
        return Err(rustler::Error::BadArg);
    }
    Ok(Some(term.decode()?))
}

fn map_get_optional_i32<'a>(env: Env<'a>, map: Term<'a>, key: &str) -> NifResult<Option<i32>> {
    let term = match map.map_get(Atom::from_str(env, key).unwrap().encode(env)) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    if term.is_atom() {
        let s = term.atom_to_string().map_err(|_| rustler::Error::BadArg)?;
        if s == "nil" {
            return Ok(None);
        }
        return Err(rustler::Error::BadArg);
    }
    Ok(Some(term.decode()?))
}

fn decode_date_fields<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyDate> {
    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }
    Ok(MontyDate {
        year: map_get_atom(env, term, "year")?.decode()?,
        month: map_get_atom(env, term, "month")?.decode()?,
        day: map_get_atom(env, term, "day")?.decode()?,
    })
}

fn decode_file_handle<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyFileHandle> {
    use std::str::FromStr;
    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }
    let path: String = map_get_atom(env, term, "path")?.decode()?;
    let mode_str: String = map_get_atom(env, term, "mode")?.decode()?;
    let mode = FileMode::from_str(&mode_str).map_err(|_| rustler::Error::BadArg)?;
    let position: u64 = map_get_atom(env, term, "position")?.decode()?;
    Ok(MontyFileHandle {
        path,
        mode,
        position,
    })
}

fn decode_datetime_fields<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyDateTime> {
    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }
    Ok(MontyDateTime {
        year: map_get_atom(env, term, "year")?.decode()?,
        month: map_get_atom(env, term, "month")?.decode()?,
        day: map_get_atom(env, term, "day")?.decode()?,
        hour: map_get_atom(env, term, "hour")?.decode()?,
        minute: map_get_atom(env, term, "minute")?.decode()?,
        second: map_get_atom(env, term, "second")?.decode()?,
        microsecond: map_get_atom(env, term, "microsecond")?.decode()?,
        offset_seconds: map_get_optional_i32(env, term, "offset_seconds")?,
        timezone_name: map_get_optional_string(env, term, "tz_name")?,
    })
}

fn decode_timedelta_fields<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyTimeDelta> {
    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }
    Ok(MontyTimeDelta {
        days: map_get_atom(env, term, "days")?.decode()?,
        seconds: map_get_atom(env, term, "seconds")?.decode()?,
        microseconds: map_get_atom(env, term, "microseconds")?.decode()?,
    })
}

fn decode_timezone_fields<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyTimeZone> {
    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }
    Ok(MontyTimeZone {
        offset_seconds: map_get_atom(env, term, "offset_seconds")?.decode()?,
        name: map_get_optional_string(env, term, "name")?,
    })
}

fn decode_named_tuple<'a>(
    env: Env<'a>,
    type_term: Term<'a>,
    fields_term: Term<'a>,
    depth: usize,
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
        let mut seen = HashSet::with_capacity(fields.len());

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

            if !seen.insert(field_name.clone()) {
                return Err(rustler::Error::Term(Box::new(format!(
                    "duplicate named tuple field: {field_name}"
                ))));
            }

            let value = decode_monty_object_depth(env, elems[1], depth + 1)?;
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

            let value = decode_monty_object_depth(env, v, depth + 1)?;
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

fn decode_dataclass<'a>(env: Env<'a>, term: Term<'a>, depth: usize) -> NifResult<MontyObject> {
    let name_key = Atom::from_str(env, "name").unwrap().encode(env);
    let name: String = term
        .map_get(name_key)
        .map_err(|_| rustler::Error::BadArg)?
        .decode()?;

    // `frozen` must be a real boolean. Previously a malformed value (e.g. the
    // atom `:bad`) was silently coerced to `false`, forging a mutable instance
    // out of garbage; now it's rejected.
    let frozen_key = Atom::from_str(env, "frozen").unwrap().encode(env);
    let frozen: bool = term
        .map_get(frozen_key)
        .map_err(|_| rustler::Error::BadArg)?
        .decode()
        .map_err(|_| invalid_dataclass("frozen must be a boolean"))?;

    // `type_id` must be absent, nil, or a non-negative integer (u64). A
    // present-but-malformed value is rejected rather than coerced to 0.
    let type_id_key = Atom::from_str(env, "type_id").unwrap().encode(env);
    let type_id: u64 = match term.map_get(type_id_key) {
        Err(_) => 0,
        Ok(t) if is_nil_atom(t) => 0, // nil / unset
        Ok(t) => t
            .decode::<u64>()
            .map_err(|_| invalid_dataclass("type_id must be a non-negative integer or nil"))?,
    };

    let fields_key = Atom::from_str(env, "fields").unwrap().encode(env);
    let fields_term = term
        .map_get(fields_key)
        .map_err(|_| rustler::Error::BadArg)?;
    let fields_iter = MapIterator::new(fields_term).ok_or(rustler::Error::BadArg)?;

    let mut pairs: Vec<(MontyObject, MontyObject)> = Vec::new();
    let mut derived_field_names: Vec<String> = Vec::new();
    for (k, v) in fields_iter {
        let key_str: String = k.decode()?;
        let val = decode_monty_object_depth(env, v, depth + 1)?;
        derived_field_names.push(key_str.clone());
        pairs.push((MontyObject::String(key_str), val));
    }

    // `field_names`, when present and non-nil, must be a proper list of
    // strings — not silently dropped back to the derived order on a decode
    // failure.
    let field_names_key = Atom::from_str(env, "field_names").unwrap().encode(env);
    let field_names: Vec<String> = match term.map_get(field_names_key) {
        Err(_) => derived_field_names.clone(),
        Ok(t) if is_nil_atom(t) => derived_field_names.clone(), // nil / unset
        Ok(t) => t
            .decode::<Vec<String>>()
            .map_err(|_| invalid_dataclass("field_names must be a list of strings or nil"))?,
    };

    let names: HashSet<&str> = field_names.iter().map(String::as_str).collect();
    let derived: HashSet<&str> = derived_field_names.iter().map(String::as_str).collect();
    if names.len() != field_names.len() || names != derived {
        return Err(invalid_dataclass(
            "field_names must contain each field exactly once",
        ));
    }

    Ok(MontyObject::Dataclass {
        name,
        type_id,
        field_names,
        attrs: DictPairs::from(pairs),
        frozen,
    })
}

fn bigint_size_error() -> rustler::Error {
    rustler::Error::Term(Box::new(
        "bigint magnitude exceeds ExMonty MAX_BIGINT_BYTES",
    ))
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
