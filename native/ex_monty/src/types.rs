use monty_types::unstable::{self, ClassTypeNode, MontyGraph, MontyNode, NodeId};
use monty_types::{
    CallArgs, FileMode, MontyComplex, MontyDate, MontyDateTime, MontyFileHandle, MontyObject,
    MontyTime, MontyTimeDelta, MontyTimeZone, MontyUuid, OsFunctionCall, ResourceLimits,
};
use num_bigint::BigInt;
use rustler::types::atom::Atom;
use rustler::types::map::MapIterator;
use rustler::types::tuple::{get_tuple, make_tuple};
use rustler::{Encoder, Env, NifResult, Term};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

/// Maximum nesting depth when converting `MontyObject` <-> Erlang term.
///
/// Untrusted Monty code can build a deeply-nested value iteratively
/// (`x = []; for _ in range(n): x = [x]`) well past Monty's own recursion
/// limit. Since monty v1 a boundary value is a flat arena, so ExMonty's half of
/// encoding one (and dropping it) no longer recurses — upstream's export of
/// the heap value into that arena still does, bounded by the interpreter's
/// recursion limit, which is why `SAFE_MAX_RECURSION_DEPTH` stays — but the *term* it becomes is a tree
/// the host then walks recursively (`inspect`, JSON encoding, `term_to_binary`,
/// `to_json_safe/1`), and decoding a host term still recurses on the small
/// dirty-scheduler native stack, where an overflow aborts the **entire BEAM**
/// (it is NOT catchable by Rustler's panic boundary). So anything deeper than
/// this is rejected with a clean error in both directions. 64 is far beyond
/// any legitimate data nesting yet leaves a large safety margin below the
/// empirically observed overflow point (~150+ frames), including headroom for
/// smaller scheduler stacks on other platforms.
const MAX_NESTING_DEPTH: usize = 64;

/// Maximum magnitude length (bytes) accepted for a `{:__bigint__, sign, bytes}`
/// tagged tuple. 1 KiB is ~2466 decimal digits — beyond any legitimate integer
/// — and caps allocation from a malicious callback payload.
const MAX_BIGINT_BYTES: usize = 1024;

fn nesting_error() -> rustler::Error {
    rustler::Error::Term(Box::new("value nesting exceeds ExMonty MAX_NESTING_DEPTH"))
}

// ── Encoding: MontyObject → Erlang Term ──────────────────────────────────────
//
// Since monty v1 a boundary value is a flat post-order arena (`MontyGraph`):
// every child id is lower than the id of the node holding it, and a
// sub-object referenced twice is ONE node referenced twice. We therefore
// encode in a single forward pass with no recursion at all — the native stack
// cost is constant however deep the value is.
//
// Sharing needs care. Erlang terms have value semantics, so a shared node is
// simply encoded once and its term reused; inside the returning process that
// stays compact. But the moment the host sends, inspects, or serializes the
// term the sharing is lost and it is expanded as a tree. Untrusted code can
// build `x = [x, x]` forty times over: 41 nodes, 2^40 leaves. So the pass
// also tracks, per node, the depth and the *expanded* (tree) size, and
// rejects a value that is too deep or would expand past
// `MAX_EXPANDED_NODES`.

/// Upper bound on the number of terms a value occupies once its shared
/// sub-objects are expanded into a tree (which is what any copy of the term
/// out of the calling process does).
const MAX_EXPANDED_NODES: u64 = 10_000_000;

/// Upper bound on the *bytes* a value occupies once expanded, when sharing is
/// what makes it large. Counting nodes alone is not enough: a single 100 KB
/// integer referenced a million times is two nodes and a million terms, yet
/// 100 GB the moment the host copies it to another process (big integers live
/// on the process heap). A value with no sharing is never rejected by this —
/// its expanded size equals what the sandbox already holds, which the
/// interpreter's own limits govern — so the effective ceiling is the larger of
/// this constant and the arena's own footprint.
const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;

fn expansion_error() -> rustler::Error {
    rustler::Error::Term(Box::new(
        "value expands past ExMonty MAX_EXPANDED_NODES (deeply shared sub-objects)",
    ))
}

fn expansion_bytes_error() -> rustler::Error {
    rustler::Error::Term(Box::new(
        "value expands past ExMonty MAX_EXPANDED_BYTES (large shared sub-objects)",
    ))
}

/// Expanded (tree) footprint of one node: term count and bytes.
#[derive(Clone, Copy, Default)]
struct Expanded {
    nodes: u64,
    bytes: u64,
}

impl Expanded {
    fn add(self, other: Self) -> Self {
        Self {
            nodes: self.nodes.saturating_add(other.nodes),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }

    fn check(self, byte_limit: u64) -> NifResult<()> {
        if self.nodes > MAX_EXPANDED_NODES {
            Err(expansion_error())
        } else if self.bytes > byte_limit {
            Err(expansion_bytes_error())
        } else {
            Ok(())
        }
    }
}

fn byte_limit(graph: &MontyGraph) -> u64 {
    MAX_EXPANDED_BYTES.max(u64::try_from(graph.decoded_size()).unwrap_or(u64::MAX))
}

fn atom<'a>(env: Env<'a>, name: &str) -> Term<'a> {
    Atom::from_str(env, name).unwrap().encode(env)
}

fn put<'a>(env: Env<'a>, map: Term<'a>, key: &str, value: Term<'a>) -> Term<'a> {
    map.map_put(atom(env, key), value).unwrap()
}

fn nil<'a>(env: Env<'a>) -> Term<'a> {
    rustler::types::atom::nil().encode(env)
}

fn optional<'a, T: Encoder>(env: Env<'a>, value: &Option<T>) -> Term<'a> {
    match value {
        Some(v) => v.encode(env),
        None => nil(env),
    }
}

pub fn encode_monty_object<'a>(env: Env<'a>, obj: &MontyObject) -> NifResult<Term<'a>> {
    let (graph, root) = unstable::graph_parts(obj);
    let (terms, _) = encode_graph(env, graph)?;
    Ok(terms[root.index()])
}

/// Encodes the positional args as a list of terms and the kwargs as a map.
pub fn encode_call_args<'a>(env: Env<'a>, args: &CallArgs) -> NifResult<(Vec<Term<'a>>, Term<'a>)> {
    let (graph, arg_ids, kwarg_ids) = unstable::call_args_parts(args);
    let (terms, sizes) = encode_graph(env, graph)?;
    // Each argument is within bounds on its own; the call as a whole must be
    // too, or `f(*[x] * 1000)` multiplies an acceptable value past them.
    let total = arg_ids
        .iter()
        .chain(kwarg_ids.iter().flat_map(|(key, value)| [key, value]))
        .fold(Expanded::default(), |total, id| {
            total.add(sizes[id.index()])
        });
    total.check(byte_limit(graph))?;
    let positional = arg_ids.iter().map(|id| terms[id.index()]).collect();
    let mut kwargs = rustler::types::map::map_new(env);
    for (key, value) in kwarg_ids {
        // kwargs keys are typically strings in Python
        kwargs = kwargs
            .map_put(terms[key.index()], terms[value.index()])
            .unwrap();
    }
    Ok((positional, kwargs))
}

/// Encodes every node of the arena, returning one term and one expanded
/// footprint per node id.
fn encode_graph<'a>(env: Env<'a>, graph: &MontyGraph) -> NifResult<(Vec<Term<'a>>, Vec<Expanded>)> {
    let nodes = graph.nodes();
    let byte_limit = byte_limit(graph);
    let alloc_error = |_| rustler::Error::Term(Box::new("out of memory encoding value"));
    let mut terms: Vec<Term<'a>> = Vec::new();
    terms.try_reserve_exact(nodes.len()).map_err(alloc_error)?;
    let mut heights: Vec<usize> = Vec::new();
    heights
        .try_reserve_exact(nodes.len())
        .map_err(alloc_error)?;
    let mut sizes: Vec<Expanded> = Vec::new();
    sizes.try_reserve_exact(nodes.len()).map_err(alloc_error)?;

    for node in nodes {
        // A leaf has height 1 and counts itself.
        let mut height = 0usize;
        let mut size = Expanded {
            nodes: 1,
            bytes: u64::try_from(node.decoded_size()).unwrap_or(u64::MAX),
        };
        node.for_each_child(|child| {
            height = height.max(heights[child.index()]);
            size = size.add(sizes[child.index()]);
        });
        let height = height + 1;
        // A root is depth 0 and depth MAX_NESTING_DEPTH is still accepted, so
        // the tallest permitted value has MAX_NESTING_DEPTH + 1 levels.
        if height > MAX_NESTING_DEPTH + 1 {
            return Err(nesting_error());
        }
        size.check(byte_limit)?;
        let term = encode_node(env, node, nodes, &terms)?;
        terms.push(term);
        heights.push(height);
        sizes.push(size);
    }
    Ok((terms, sizes))
}

fn encode_float<'a>(env: Env<'a>, f: f64) -> Term<'a> {
    if f.is_nan() {
        atom(env, "nan")
    } else if f.is_infinite() {
        if f.is_sign_positive() {
            atom(env, "infinity")
        } else {
            atom(env, "neg_infinity")
        }
    } else {
        f.encode(env)
    }
}

fn collect<'a>(terms: &[Term<'a>], ids: &[NodeId]) -> Vec<Term<'a>> {
    ids.iter().map(|id| terms[id.index()]).collect()
}

/// Encodes one node; `terms` holds the already-encoded lower ids.
fn encode_node<'a>(
    env: Env<'a>,
    node: &MontyNode,
    nodes: &[MontyNode],
    terms: &[Term<'a>],
) -> NifResult<Term<'a>> {
    let term = match node {
        MontyNode::None => nil(env),
        MontyNode::NotImplemented => atom(env, "not_implemented"),
        MontyNode::Ellipsis => atom(env, "ellipsis"),
        MontyNode::Bool(b) => b.encode(env),
        MontyNode::Int(i) => i.encode(env),
        MontyNode::BigInt(bi) => bi.encode(env),
        MontyNode::Float(f) => encode_float(env, *f),
        MontyNode::Complex(c) => make_tuple(
            env,
            &[
                atom(env, "complex"),
                encode_float(env, c.real),
                encode_float(env, c.imag),
            ],
        ),
        MontyNode::String(s) => s.encode(env),
        MontyNode::Bytes(b) => {
            let mut owned = rustler::OwnedBinary::new(b.len()).ok_or_else(|| {
                rustler::Error::Term(Box::new("failed to allocate binary for bytes value"))
            })?;
            owned.as_mut_slice().copy_from_slice(b);
            let binary = owned.release(env);
            make_tuple(env, &[atom(env, "bytes"), binary.encode(env)])
        }
        MontyNode::Decimal(text) => make_tuple(env, &[atom(env, "decimal"), text.encode(env)]),
        MontyNode::List(ids) => collect(terms, ids).encode(env),
        MontyNode::Tuple(ids) => make_tuple(env, &collect(terms, ids)),
        MontyNode::Dict(pairs) => {
            let mut map = rustler::types::map::map_new(env);
            for (key, value) in pairs {
                map = map
                    .map_put(terms[key.index()], terms[value.index()])
                    .unwrap();
            }
            map
        }
        MontyNode::Set(ids) | MontyNode::FrozenSet(ids) => encode_mapset(env, &collect(terms, ids)),
        MontyNode::Path(p) => make_tuple(env, &[atom(env, "path"), p.encode(env)]),
        MontyNode::FileHandle(handle) => {
            let map = rustler::types::map::map_new(env);
            let map = put(env, map, "path", handle.path.encode(env));
            let map = put(env, map, "mode", handle.mode.as_str().encode(env));
            let map = put(env, map, "position", handle.position.encode(env));
            make_tuple(env, &[atom(env, "file_handle"), map])
        }
        MontyNode::NamedTuple {
            type_name,
            field_names,
            values,
        } => {
            let fields: Vec<Term> = field_names
                .iter()
                .zip(values.iter())
                .map(|(name, id)| make_tuple(env, &[name.encode(env), terms[id.index()]]))
                .collect();
            make_tuple(
                env,
                &[
                    atom(env, "named_tuple"),
                    type_name.encode(env),
                    fields.encode(env),
                ],
            )
        }
        MontyNode::ClassInstance {
            class_type,
            instance_id,
            attrs,
        } => {
            let MontyNode::ClassType(class) = &nodes[class_type.index()] else {
                return Err(rustler::Error::Term(Box::new(
                    "malformed class instance: class is not a class type",
                )));
            };
            let mut fields = rustler::types::map::map_new(env);
            let mut field_names: Vec<Term> = Vec::with_capacity(attrs.len());
            for (key, value) in attrs {
                // Attribute names are strings; a non-string key (possible only
                // in host-built input) has no field name and is skipped.
                if let MontyNode::String(_) = &nodes[key.index()] {
                    let key_term = terms[key.index()];
                    fields = fields.map_put(key_term, terms[value.index()]).unwrap();
                    field_names.push(key_term);
                }
            }
            let map = rustler::types::map::map_new(env);
            let map = put(
                env,
                map,
                "__struct__",
                atom(env, "Elixir.ExMonty.Dataclass"),
            );
            let map = put(env, map, "name", class.name.encode(env));
            let map = put(env, map, "fields", fields);
            let map = put(env, map, "field_names", field_names.encode(env));
            let map = put(env, map, "type_id", class.id.to_string().encode(env));
            let map = put(env, map, "id", instance_id.to_string().encode(env));
            // Provenance: a sandbox-defined instance must not be mistaken
            // for an object the host supplied if it is handed back.
            let origin = if class.host_defined {
                "host"
            } else {
                "sandbox"
            };
            let map = put(env, map, "origin", atom(env, origin));
            // Upstream no longer has a frozen flag at the boundary.
            put(env, map, "frozen", false.encode(env))
        }
        // Class and module objects have no Elixir-side value; like every other
        // value without a faithful mapping they surface as their repr.
        MontyNode::ClassType(class) => {
            let repr = format!("<class '{}'>", class.name);
            make_tuple(env, &[atom(env, "repr"), repr.encode(env)])
        }
        MontyNode::Module { name, .. } => {
            let repr = format!("<module '{name}'>");
            make_tuple(env, &[atom(env, "repr"), repr.encode(env)])
        }
        MontyNode::Exception { exc_type, arg } => {
            let type_str = exc_type.to_string();
            let map = rustler::types::map::map_new(env);
            let map = put(
                env,
                map,
                "__struct__",
                atom(env, "Elixir.ExMonty.Exception"),
            );
            let map = put(env, map, "type", atom(env, &snake_case(&type_str)));
            let map = put(env, map, "message", optional(env, arg));
            put(env, map, "traceback", Vec::<Term>::new().encode(env))
        }
        MontyNode::Type(ty) => {
            let repr = ty.to_string();
            Atom::from_str(env, &snake_case(&repr))
                .unwrap_or_else(|_| Atom::from_str(env, "unknown_type").unwrap())
                .encode(env)
        }
        MontyNode::BuiltinFunction(_) => atom(env, "builtin_function"),
        MontyNode::Repr(s) => make_tuple(env, &[atom(env, "repr"), s.encode(env)]),
        MontyNode::Cycle(placeholder) => {
            make_tuple(env, &[atom(env, "cycle"), placeholder.encode(env)])
        }
        MontyNode::Function {
            name, docstring, ..
        } => make_tuple(
            env,
            &[
                atom(env, "function"),
                name.encode(env),
                optional(env, docstring),
            ],
        ),
        MontyNode::Date(d) => encode_date(env, d),
        MontyNode::DateTime(dt) => encode_datetime(env, dt),
        MontyNode::Time(t) => encode_time(env, t),
        MontyNode::TimeDelta(td) => encode_timedelta(env, td),
        MontyNode::TimeZone(tz) => encode_timezone(env, tz),
    };
    Ok(term)
}

fn encode_time<'a>(env: Env<'a>, t: &MontyTime) -> Term<'a> {
    let map = rustler::types::map::map_new(env);
    let map = put(env, map, "hour", t.hour.encode(env));
    let map = put(env, map, "minute", t.minute.encode(env));
    let map = put(env, map, "second", t.second.encode(env));
    let map = put(env, map, "microsecond", t.microsecond.encode(env));
    let map = put(env, map, "offset_seconds", optional(env, &t.offset_seconds));
    let map = put(env, map, "tz_name", optional(env, &t.timezone_name));
    let map = put(env, map, "fold", t.fold.encode(env));
    make_tuple(env, &[atom(env, "time"), map])
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
//
// Erlang terms are trees, so decoding recurses (bounded by
// `MAX_NESTING_DEPTH`) and pushes each node straight into one arena, children
// first, which is the post-order the arena requires.

/// A host-defined class instance decoded from an `%ExMonty.Dataclass{}`, keyed
/// by its instance id. Method calls the sandbox makes on such an instance
/// arrive with only this id, so the run keeps the host's copy to hand back as
/// `self` (see `objects.rs`).
pub type HostObject = (MontyUuid, MontyObject);

pub struct Decoded {
    pub object: MontyObject,
    pub host_objects: Vec<HostObject>,
}

pub fn decode_monty_object<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<Decoded> {
    let mut decoder = Decoder {
        env,
        graph: MontyGraph::new(),
        instances: Vec::new(),
    };
    let root = decoder.node(term, 0)?;
    decoder.finish(root)
}

struct Decoder<'a> {
    env: Env<'a>,
    graph: MontyGraph,
    /// Every decoded dataclass instance: its id and its node.
    instances: Vec<(MontyUuid, NodeId)>,
}

impl<'a> Decoder<'a> {
    fn finish(self, root: NodeId) -> NifResult<Decoded> {
        let host_objects = self
            .instances
            .iter()
            .map(|(id, node)| (*id, self.graph.value(*node).to_owned()))
            .collect();
        let object = unstable::object_from_graph(self.graph, root)
            .map_err(|e| rustler::Error::Term(Box::new(format!("invalid value: {e}"))))?;
        Ok(Decoded {
            object,
            host_objects,
        })
    }

    fn push(&mut self, node: MontyNode) -> NodeId {
        self.graph.push(node)
    }

    fn nodes(&mut self, terms: &[Term<'a>], depth: usize) -> NifResult<Vec<NodeId>> {
        terms.iter().map(|t| self.node(*t, depth + 1)).collect()
    }

    fn node(&mut self, term: Term<'a>, depth: usize) -> NifResult<NodeId> {
        if depth > MAX_NESTING_DEPTH {
            return Err(nesting_error());
        }
        let env = self.env;

        // nil, true, false, ellipsis atoms
        if term.is_atom() {
            let atom_str: String = term.atom_to_string().map_err(|_| rustler::Error::BadArg)?;
            let node = match atom_str.as_str() {
                "nil" => MontyNode::None,
                "true" => MontyNode::Bool(true),
                "false" => MontyNode::Bool(false),
                "ellipsis" => MontyNode::Ellipsis,
                "not_implemented" => MontyNode::NotImplemented,
                "infinity" => MontyNode::Float(f64::INFINITY),
                "neg_infinity" => MontyNode::Float(f64::NEG_INFINITY),
                "nan" => MontyNode::Float(f64::NAN),
                other => MontyNode::String(other.to_owned()),
            };
            return Ok(self.push(node));
        }

        // Try i64 first (most common integer case)
        if let Ok(i) = term.decode::<i64>() {
            return Ok(self.push(MontyNode::Int(i)));
        }

        // Big integer (arbitrary precision)
        if let Ok(bi) = term.decode::<BigInt>() {
            if bi.bits() > (MAX_BIGINT_BYTES as u64) * 8 {
                return Err(bigint_size_error());
            }
            return Ok(self.push(MontyNode::BigInt(bi)));
        }

        // Float
        if term.is_float() {
            let f: f64 = term.decode()?;
            return Ok(self.push(MontyNode::Float(f)));
        }

        // Binary/String
        if term.is_binary() {
            if let Ok(s) = term.decode::<String>() {
                return Ok(self.push(MontyNode::String(s)));
            }

            let binary: rustler::Binary = term.decode()?;
            return Ok(self.push(MontyNode::Bytes(binary.as_slice().to_vec())));
        }

        // Tuple - check for tagged tuples first
        if let Ok(elements) = get_tuple(term) {
            if let Some(Ok(tag)) = elements.first().map(|t| t.atom_to_string()) {
                if let Some(node) = self.tagged(&tag, &elements, depth)? {
                    return Ok(node);
                }
            }
            // Regular tuple
            let items = self.nodes(&elements, depth)?;
            return Ok(self.push(MontyNode::Tuple(items)));
        }

        // List
        if term.is_list() {
            let list: Vec<Term> = term.decode()?;
            let items = self.nodes(&list, depth)?;
            return Ok(self.push(MontyNode::List(items)));
        }

        // Map - check for MapSet / Dataclass structs
        if term.is_map() {
            if let Ok(struct_val) = term.map_get(atom(env, "__struct__")) {
                if let Ok(struct_name) = struct_val.atom_to_string() {
                    if struct_name == "Elixir.MapSet" {
                        let inner_map = term
                            .map_get(atom(env, "map"))
                            .map_err(|_| rustler::Error::BadArg)?;
                        let iter = MapIterator::new(inner_map).ok_or(rustler::Error::BadArg)?;
                        let members: Vec<Term> = iter.map(|(k, _v)| k).collect();
                        let items = self.nodes(&members, depth)?;
                        return Ok(self.push(MontyNode::Set(items)));
                    }
                    if struct_name == "Elixir.ExMonty.Dataclass" {
                        return self.dataclass(term, depth);
                    }
                }
            }
            // Regular map → Dict
            let iter = MapIterator::new(term).ok_or(rustler::Error::BadArg)?;
            let mut pairs = Vec::new();
            for (k, v) in iter {
                let key = self.node(k, depth + 1)?;
                let value = self.node(v, depth + 1)?;
                pairs.push((key, value));
            }
            return Ok(self.push(MontyNode::Dict(pairs)));
        }

        Err(rustler::Error::BadArg)
    }

    /// Decodes a tagged tuple, or returns `None` when the tuple is not one of
    /// the recognised tagged forms and should be decoded as a plain tuple.
    fn tagged(
        &mut self,
        tag: &str,
        elements: &[Term<'a>],
        depth: usize,
    ) -> NifResult<Option<NodeId>> {
        let env = self.env;
        let node = match (tag, elements.len()) {
            // {:named_tuple, type_name, fields}
            ("named_tuple", 3) => {
                return self.named_tuple(elements[1], elements[2], depth).map(Some)
            }
            ("bytes", 2) => {
                let binary: rustler::Binary = elements[1].decode()?;
                MontyNode::Bytes(binary.as_slice().to_vec())
            }
            ("path", 2) => MontyNode::Path(elements[1].decode()?),
            ("file_handle", 2) => MontyNode::FileHandle(decode_file_handle(env, elements[1])?),
            ("repr", 2) => MontyNode::Repr(elements[1].decode()?),
            ("decimal", 2) => MontyNode::Decimal(elements[1].decode()?),
            ("function", 2) => MontyNode::Function {
                name: elements[1].decode()?,
                docstring: None,
                is_async: false,
            },
            // {:function, name, docstring}
            ("function", 3) => {
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
                MontyNode::Function {
                    name,
                    docstring,
                    is_async: false,
                }
            }
            // {:complex, real, imag}
            ("complex", 3) => MontyNode::Complex(MontyComplex {
                real: decode_float_part(elements[1])?,
                imag: decode_float_part(elements[2])?,
            }),
            ("date", 2) => MontyNode::Date(decode_date_fields(env, elements[1])?),
            ("datetime", 2) => MontyNode::DateTime(decode_datetime_fields(env, elements[1])?),
            ("time", 2) => MontyNode::Time(decode_time_fields(env, elements[1])?),
            ("timedelta", 2) => MontyNode::TimeDelta(decode_timedelta_fields(env, elements[1])?),
            ("timezone", 2) => MontyNode::TimeZone(decode_timezone_fields(env, elements[1])?),
            // bigint tagged tuple {:__bigint__, sign, bytes}
            ("__bigint__", 3) => {
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
                MontyNode::BigInt(BigInt::from_bytes_be(num_sign, bytes))
            }
            _ => return Ok(None),
        };
        Ok(Some(self.push(node)))
    }

    fn named_tuple(
        &mut self,
        type_term: Term<'a>,
        fields_term: Term<'a>,
        depth: usize,
    ) -> NifResult<NodeId> {
        let raw_type_name = decode_name(type_term)?;
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

                let field_name = decode_name(elems[0])?;
                if !seen.insert(field_name.clone()) {
                    return Err(rustler::Error::Term(Box::new(format!(
                        "duplicate named tuple field: {field_name}"
                    ))));
                }

                let value = self.node(elems[1], depth + 1)?;
                field_names.push(field_name);
                values.push(value);
            }

            return Ok(self.push(MontyNode::NamedTuple {
                type_name,
                field_names,
                values,
            }));
        }

        if fields_term.is_map() {
            let iter = MapIterator::new(fields_term).ok_or(rustler::Error::BadArg)?;

            let mut by_name: HashMap<String, NodeId> = HashMap::new();
            for (k, v) in iter {
                let field_name = decode_name(k)?;
                let value = self.node(v, depth + 1)?;
                by_name.insert(field_name, value);
            }

            let (field_names, values) = order_named_tuple_fields(&type_name, by_name)?;

            return Ok(self.push(MontyNode::NamedTuple {
                type_name,
                field_names,
                values,
            }));
        }

        Err(rustler::Error::BadArg)
    }

    /// `%ExMonty.Dataclass{}` → a host-defined class instance.
    fn dataclass(&mut self, term: Term<'a>, depth: usize) -> NifResult<NodeId> {
        let env = self.env;
        let name: String = term
            .map_get(atom(env, "name"))
            .map_err(|_| rustler::Error::BadArg)?
            .decode()?;

        // monty v1 has no frozen flag at the boundary: attribute writes on a
        // host instance always succeed on the sandbox's own copy. Accepting
        // `frozen: true` would pretend immutability is enforced when it isn't,
        // so it is an error; a present value must still be a real boolean.
        if let Ok(frozen) = term.map_get(atom(env, "frozen")) {
            let frozen = frozen
                .decode::<bool>()
                .map_err(|_| invalid_dataclass("frozen must be a boolean"))?;
            if frozen {
                return Err(invalid_dataclass(
                    "frozen dataclasses are no longer supported (monty v1 removed the flag); use frozen: false",
                ));
            }
        }

        // Class identity. Absent/nil derives a stable id from the class name,
        // so same-named dataclasses are one class inside the sandbox. A uuid
        // string (what encoding produces) is used as is; a legacy integer is
        // mixed into the derived id.
        let class_id = match term.map_get(atom(env, "type_id")) {
            Err(_) => derived_class_uuid(&name, None),
            Ok(t) if is_nil_atom(t) => derived_class_uuid(&name, None),
            Ok(t) if t.is_binary() => decode_uuid(t).ok_or_else(|| {
                invalid_dataclass("type_id must be a uuid string, integer or nil")
            })?,
            Ok(t) => {
                let legacy = t.decode::<u64>().map_err(|_| {
                    invalid_dataclass("type_id must be a uuid string, integer or nil")
                })?;
                derived_class_uuid(&name, Some(legacy))
            }
        };

        // Instance identity: reuse the id the struct carries (so a value the
        // sandbox handed out and the host hands back stays the same object),
        // otherwise mint a fresh one.
        let instance_id = match term.map_get(atom(env, "id")) {
            Err(_) => random_uuid()?,
            Ok(t) if is_nil_atom(t) => random_uuid()?,
            Ok(t) => decode_uuid(t)
                .ok_or_else(|| invalid_dataclass("id must be a uuid string or nil"))?,
        };

        let fields_term = term
            .map_get(atom(env, "fields"))
            .map_err(|_| rustler::Error::BadArg)?;
        let fields_iter = MapIterator::new(fields_term).ok_or(rustler::Error::BadArg)?;

        let mut values: HashMap<String, NodeId> = HashMap::new();
        let mut derived_field_names: Vec<String> = Vec::new();
        for (k, v) in fields_iter {
            let key: String = k.decode()?;
            let value = self.node(v, depth + 1)?;
            derived_field_names.push(key.clone());
            values.insert(key, value);
        }

        // `field_names`, when present and non-nil, must be a proper list of
        // strings naming each field exactly once; it fixes the attribute
        // order the sandbox sees.
        let field_names: Vec<String> = match term.map_get(atom(env, "field_names")) {
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

        let host_origin = match term.map_get(atom(env, "origin")) {
            Err(_) => true,
            Ok(t) if is_nil_atom(t) => true,
            Ok(t) => match t.atom_to_string().ok().as_deref() {
                Some("host") => true,
                Some("sandbox") => false,
                _ => return Err(invalid_dataclass("origin must be :host or :sandbox")),
            },
        };

        let mut attrs = Vec::with_capacity(field_names.len());
        for field in field_names {
            let value = values[&field];
            let key = self.push(MontyNode::String(field));
            attrs.push((key, value));
        }

        let class_type = self.push(MontyNode::ClassType(Box::new(ClassTypeNode {
            name,
            id: class_id,
            host_defined: true,
            is_dataclass: true,
            attrs: Vec::new(),
        })));
        let node = self.push(MontyNode::ClassInstance {
            class_type,
            instance_id,
            attrs,
        });
        // Only objects the host supplied are tracked for method routing. A
        // sandbox-defined instance that comes back resolves to the original
        // by id while it is alive; once it is gone it is a plain data copy,
        // and must not let the sandbox route method calls to the host under
        // a class name of its own choosing.
        if host_origin {
            self.instances.push((instance_id, node));
        }
        Ok(node)
    }
}

/// An atom or binary used as a name.
fn decode_name(term: Term) -> NifResult<String> {
    if term.is_atom() {
        term.atom_to_string().map_err(|_| rustler::Error::BadArg)
    } else if term.is_binary() {
        term.decode()
    } else {
        Err(rustler::Error::BadArg)
    }
}

/// One part of a `{:complex, real, imag}`: a float, an integer, or one of the
/// non-finite float atoms.
fn decode_float_part(term: Term) -> NifResult<f64> {
    if term.is_atom() {
        return match term.atom_to_string().as_deref() {
            Ok("infinity") => Ok(f64::INFINITY),
            Ok("neg_infinity") => Ok(f64::NEG_INFINITY),
            Ok("nan") => Ok(f64::NAN),
            _ => Err(rustler::Error::BadArg),
        };
    }
    if let Ok(f) = term.decode::<f64>() {
        return Ok(f);
    }
    term.decode::<i64>().map(|i| i as f64)
}

fn decode_uuid(term: Term) -> Option<MontyUuid> {
    let text: String = term.decode().ok()?;
    MontyUuid::parse(&text)
}

fn random_uuid() -> NifResult<MontyUuid> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| {
        rustler::Error::Term(Box::new("failed to obtain entropy for a dataclass id"))
    })?;
    Ok(MontyUuid::from_random_bytes(bytes))
}

/// A stable class id for a dataclass the host did not give one: FNV-1a (128
/// bit) of the class name, so it is identical across runs, nodes and builds.
fn derived_class_uuid(name: &str, legacy_type_id: Option<u64>) -> MontyUuid {
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    let mut hash = OFFSET_BASIS;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(b"ex_monty.dataclass\0");
    feed(name.as_bytes());
    if let Some(id) = legacy_type_id {
        feed(b"\0");
        feed(&id.to_be_bytes());
    }
    MontyUuid::from_random_bytes(hash.to_be_bytes())
}

// ── Helper: Decode named inputs ──────────────────────────────────────────────

pub fn decode_inputs<'a>(
    env: Env<'a>,
    inputs: Vec<(String, Term<'a>)>,
    expected_input_names: &[String],
) -> NifResult<(Vec<MontyObject>, Vec<HostObject>)> {
    if expected_input_names.is_empty() {
        if inputs.is_empty() {
            return Ok((Vec::new(), Vec::new()));
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

    let mut host_objects: Vec<HostObject> = Vec::new();
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

        let decoded = decode_monty_object(env, term)?;
        host_objects.extend(decoded.host_objects);
        provided.insert(name, decoded.object);
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

    Ok((ordered, host_objects))
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
        limits = limits.max_feed_duration(duration);
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
        OsFunctionCall::CreateExclusive(_) => "create_exclusive",
        OsFunctionCall::Mkdir(_) => "mkdir",
        OsFunctionCall::Unlink(_) => "unlink",
        OsFunctionCall::Rmdir(_) => "rmdir",
        OsFunctionCall::Iterdir(_) => "iterdir",
        OsFunctionCall::Scan(_) => "scan",
        OsFunctionCall::Stat(_) => "stat",
        OsFunctionCall::Rename(_) => "rename",
        OsFunctionCall::Resolve(_) => "resolve",
        OsFunctionCall::Absolute(_) => "absolute",
        OsFunctionCall::Getenv(_) => "getenv",
        OsFunctionCall::GetEnviron => "get_environ",
        OsFunctionCall::DateToday => "date_today",
        OsFunctionCall::DateTimeNow(_) => "datetime_now",
        OsFunctionCall::Urandom(_) => "urandom",
        OsFunctionCall::Time(_) => "time",
        // ExMonty runs every sandbox with `SleepMode::CallHost`, so the
        // `System*` variants are not produced; they map to the same names in
        // case a snapshot from a differently-configured producer is loaded.
        OsFunctionCall::Sleep(_) | OsFunctionCall::SystemSleep(_) => "sleep",
        OsFunctionCall::AsyncSleep(_) | OsFunctionCall::AsyncSystemSleep(_) => "async_sleep",
        OsFunctionCall::Import(_) => "import",
        OsFunctionCall::ImportStubs(_) => "import_stubs",
        OsFunctionCall::GetTypeHints(_) => "get_type_hints",
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

fn decode_time_fields<'a>(env: Env<'a>, term: Term<'a>) -> NifResult<MontyTime> {
    if !term.is_map() {
        return Err(rustler::Error::BadArg);
    }
    let fold = match term.map_get(atom(env, "fold")) {
        Ok(t) if !is_nil_atom(t) => t.decode()?,
        _ => 0,
    };
    Ok(MontyTime {
        hour: map_get_atom(env, term, "hour")?.decode()?,
        minute: map_get_atom(env, term, "minute")?.decode()?,
        second: map_get_atom(env, term, "second")?.decode()?,
        microsecond: map_get_atom(env, term, "microsecond")?.decode()?,
        offset_seconds: map_get_optional_i32(env, term, "offset_seconds")?,
        timezone_name: map_get_optional_string(env, term, "tz_name")?,
        fold,
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
    mut by_name: HashMap<String, NodeId>,
) -> NifResult<(Vec<String>, Vec<NodeId>)> {
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
