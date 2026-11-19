//! The host objects a run has been handed.
//!
//! An `%ExMonty.Dataclass{}` crosses into the sandbox as an instance of a
//! host-defined class. The sandbox works on its own copy of the attributes,
//! but a *method call* on such an instance is not something the sandbox can
//! answer: it suspends with only the instance id and expects the host to know
//! the object. ExMonty's API hands the host the receiver itself (`self` as the
//! first argument), so each run remembers the host's copy of every instance it
//! was given, keyed by id.
//!
//! The copy is what the host last sent for that id. Attribute writes made
//! inside the sandbox live on the sandbox's copy and are not reflected here.

use std::collections::{HashMap, VecDeque};

use monty_types::unstable::{self, MontyNode};
use monty_types::{MontyObject, MontyUuid};
use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::output::OutputBudget;

/// At most this many host objects are remembered per run; the oldest are
/// forgotten first. A method call on a forgotten object raises `RuntimeError`
/// in the sandbox.
const MAX_OBJECTS: usize = 10_000;

/// Upper bound on the summed decoded size of the remembered objects.
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct HostObjects {
    by_id: HashMap<MontyUuid, (MontyObject, usize)>,
    /// Insertion order, oldest first. May hold ids already replaced or
    /// evicted; those are skipped on eviction.
    order: VecDeque<MontyUuid>,
    bytes: usize,
}

impl HostObjects {
    pub fn get(&self, id: &MontyUuid) -> Option<&MontyObject> {
        self.by_id.get(id).map(|(object, _)| object)
    }

    pub fn extend(&mut self, objects: impl IntoIterator<Item = (MontyUuid, MontyObject)>) {
        for (id, object) in objects {
            self.insert(id, object);
        }
    }

    pub fn insert(&mut self, id: MontyUuid, object: MontyObject) {
        let (graph, _) = unstable::graph_parts(&object);
        let size = graph.decoded_size();
        if let Some((_, old_size)) = self.by_id.remove(&id) {
            self.bytes -= old_size;
            self.order.retain(|existing| existing != &id);
        }
        if size > MAX_BYTES {
            return;
        }
        while self.by_id.len() >= MAX_OBJECTS || self.bytes + size > MAX_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, old_size)) = self.by_id.remove(&oldest) {
                self.bytes -= old_size;
            }
        }
        self.by_id.insert(id, (object, size));
        self.order.push_back(id);
        self.bytes += size;
    }

    /// The class name of a remembered instance.
    pub fn class_name(object: &MontyObject) -> Option<&str> {
        let (graph, root) = unstable::graph_parts(object);
        let MontyNode::ClassInstance { class_type, .. } = graph.node(root) else {
            return None;
        };
        match graph.node(*class_type) {
            MontyNode::ClassType(class) => Some(&class.name),
            _ => None,
        }
    }
}

// Serialized as the ordered list of `(id, object)` pairs, oldest first, and
// rebuilt through `insert` so the caps hold for loaded data too.
impl Serialize for HostObjects {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.by_id.len()))?;
        for id in &self.order {
            if let Some((object, _)) = self.by_id.get(id) {
                seq.serialize_element(&(id, object))?;
            }
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for HostObjects {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectsVisitor;

        impl<'de> Visitor<'de> for ObjectsVisitor {
            type Value = HostObjects;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a sequence of (id, object) pairs")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<HostObjects, A::Error> {
                let mut objects = HostObjects::default();
                while let Some((id, object)) = seq.next_element::<(MontyUuid, MontyObject)>()? {
                    objects.insert(id, object);
                }
                Ok(objects)
            }
        }

        deserializer.deserialize_seq(ObjectsVisitor)
    }
}

/// Everything a run carries from one suspension to the next besides the
/// interpreter state itself. It travels with the snapshot — including through
/// dump/load — so neither the output ceiling nor the host objects reset.
#[derive(Serialize, Deserialize)]
pub struct RunState {
    pub output_budget: OutputBudget,
    pub objects: HostObjects,
}

impl RunState {
    pub fn new(output_budget: OutputBudget) -> Self {
        Self {
            output_budget,
            objects: HostObjects::default(),
        }
    }
}
