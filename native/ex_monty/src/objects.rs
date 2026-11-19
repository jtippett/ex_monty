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
    by_id: HashMap<MontyUuid, Entry>,
    /// Insertion order, oldest first, as `(id, generation)`. A slot is live
    /// only while its generation matches the entry's: replacing or evicting
    /// an object leaves a stale slot behind instead of paying a linear scan.
    order: VecDeque<(MontyUuid, u64)>,
    bytes: usize,
    next_generation: u64,
}

struct Entry {
    object: MontyObject,
    size: usize,
    generation: u64,
}

impl HostObjects {
    pub fn get(&self, id: &MontyUuid) -> Option<&MontyObject> {
        self.by_id.get(id).map(|entry| &entry.object)
    }

    pub fn extend(&mut self, objects: impl IntoIterator<Item = (MontyUuid, MontyObject)>) {
        for (id, object) in objects {
            self.insert(id, object);
        }
    }

    fn is_live(&self, slot: &(MontyUuid, u64)) -> bool {
        self.by_id
            .get(&slot.0)
            .is_some_and(|entry| entry.generation == slot.1)
    }

    pub fn insert(&mut self, id: MontyUuid, object: MontyObject) {
        let (graph, _) = unstable::graph_parts(&object);
        let size = graph.decoded_size();
        if let Some(old) = self.by_id.remove(&id) {
            self.bytes -= old.size;
        }
        if size > MAX_BYTES {
            return;
        }
        while self.by_id.len() >= MAX_OBJECTS || self.bytes + size > MAX_BYTES {
            let Some(slot) = self.order.pop_front() else {
                break;
            };
            if self.is_live(&slot) {
                if let Some(old) = self.by_id.remove(&slot.0) {
                    self.bytes -= old.size;
                }
            }
        }
        // Stale slots are bounded: drop them once they outnumber live ones.
        if self.order.len() >= 2 * MAX_OBJECTS {
            let by_id = &self.by_id;
            self.order.retain(|slot| {
                by_id
                    .get(&slot.0)
                    .is_some_and(|entry| entry.generation == slot.1)
            });
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.by_id.insert(
            id,
            Entry {
                object,
                size,
                generation,
            },
        );
        self.order.push_back((id, generation));
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
        // Exactly one live slot exists per entry, so the count is exact.
        let mut seq = serializer.serialize_seq(Some(self.by_id.len()))?;
        for slot in self.order.iter().filter(|slot| self.is_live(slot)) {
            seq.serialize_element(&(&slot.0, &self.by_id[&slot.0].object))?;
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
