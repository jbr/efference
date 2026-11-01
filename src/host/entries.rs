//! The host's session map: one entry per id, starting or hosted
//! (docs/control-api.md).
//!
//! A creation or a resume puts its id here as [`Entry::Starting`] before anything is
//! prepared, and adoption replaces it with [`Entry::Hosted`]. The accessors name which
//! kind they read, so a lookup that means a running session cannot be answered by one
//! that is still starting, and one that means "is this id taken" sees both.

use super::*;
use crate::host::starting::Starting;

/// One id's entry.
// One entry per session on the host, so the size of the larger variant is never
// multiplied by anything that would make boxing it pay.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum Entry {
    /// Reserved and being prepared (`starting.rs`).
    Starting(Arc<Starting>),
    /// Adopted: a runtime is running it.
    Hosted(Hosted),
}

/// The map, keyed by session id and sorted by it.
#[derive(Default)]
pub(crate) struct Sessions(BTreeMap<String, Entry>);

impl Sessions {
    pub(crate) fn entry(&self, id: &str) -> Option<&Entry> {
        self.0.get(id)
    }

    pub(crate) fn ids(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    pub(crate) fn hosted(&self, id: &str) -> Option<&Hosted> {
        match self.0.get(id) {
            Some(Entry::Hosted(hosted)) => Some(hosted),
            _ => None,
        }
    }

    pub(crate) fn hosted_mut(&mut self, id: &str) -> Option<&mut Hosted> {
        match self.0.get_mut(id) {
            Some(Entry::Hosted(hosted)) => Some(hosted),
            _ => None,
        }
    }

    /// Every hosted session, by id.
    pub(crate) fn hosted_iter(&self) -> impl Iterator<Item = (&String, &Hosted)> {
        self.0.iter().filter_map(|(id, entry)| match entry {
            Entry::Hosted(hosted) => Some((id, hosted)),
            Entry::Starting(_) => None,
        })
    }

    pub(crate) fn hosted_values_mut(&mut self) -> impl Iterator<Item = &mut Hosted> {
        self.0.values_mut().filter_map(|entry| match entry {
            Entry::Hosted(hosted) => Some(hosted),
            Entry::Starting(_) => None,
        })
    }

    /// Every starting session.
    pub(crate) fn starting_iter(&self) -> impl Iterator<Item = &Arc<Starting>> {
        self.0.values().filter_map(|entry| match entry {
            Entry::Starting(starting) => Some(starting),
            Entry::Hosted(_) => None,
        })
    }

    /// Put `starting` under its id, unless the id is taken.
    pub(crate) fn insert_starting(&mut self, starting: &Arc<Starting>) -> bool {
        if self.0.contains_key(starting.id()) {
            return false;
        }
        self.0.insert(
            starting.id().to_string(),
            Entry::Starting(Arc::clone(starting)),
        );
        true
    }

    /// Put `hosted` under `id`: into a free id, or in place of the starting entry it
    /// was prepared from. Any other occupant refuses.
    pub(crate) fn insert_hosted(
        &mut self,
        id: &str,
        hosted: Hosted,
        from: Option<&Arc<Starting>>,
    ) -> bool {
        let replaces = match (self.0.get(id), from) {
            (None, _) => true,
            (Some(Entry::Starting(occupant)), Some(from)) => Arc::ptr_eq(occupant, from),
            _ => false,
        };
        if replaces {
            self.0.insert(id.to_string(), Entry::Hosted(hosted));
        }
        replaces
    }

    /// Remove `starting`'s entry, if it is still the one under its id.
    pub(crate) fn remove_starting(&mut self, starting: &Arc<Starting>) -> bool {
        let ours = matches!(
            self.0.get(starting.id()),
            Some(Entry::Starting(occupant)) if Arc::ptr_eq(occupant, starting)
        );
        if ours {
            self.0.remove(starting.id());
        }
        ours
    }

    /// Remove the hosted session under `id`, unless an end under way on it holds the
    /// removal back (`Departure`, `end.rs`): then the entry stays, and this is `false`.
    pub(crate) fn remove_hosted(&mut self, id: &str) -> bool {
        match self.0.get(id) {
            Some(Entry::Hosted(hosted)) if hosted.departure.held() => false,
            Some(Entry::Hosted(_)) => {
                self.0.remove(id);
                true
            }
            _ => true,
        }
    }
}
