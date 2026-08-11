//! Type-keyed application state.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// A map from type to a single shared value of that type.
///
/// Registering two values of the same type replaces the first; state is
/// identified by type, exactly like FastAPI's dependency overrides but resolved
/// at compile time on the reading side.
#[derive(Default)]
pub struct StateMap {
    entries: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    /// Kept for diagnostics: `State<T>` for an unregistered `T` should be able
    /// to say what *is* registered.
    names: Vec<&'static str>,
}

impl StateMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a value, returning `true` if it replaced an existing one.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> bool {
        let replaced = self
            .entries
            .insert(TypeId::of::<T>(), Arc::new(value))
            .is_some();
        if !replaced {
            self.names.push(std::any::type_name::<T>());
        }
        replaced
    }

    /// Registers an already-shared value.
    pub fn insert_arc<T: Send + Sync + 'static>(&mut self, value: Arc<T>) -> bool {
        let replaced = self.entries.insert(TypeId::of::<T>(), value).is_some();
        if !replaced {
            self.names.push(std::any::type_name::<T>());
        }
        replaced
    }

    /// Looks up a value by type.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.entries
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|any| any.downcast::<T>().ok())
    }

    /// The type names of everything registered, for error messages.
    pub fn registered(&self) -> &[&'static str] {
        &self.names
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl std::fmt::Debug for StateMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateMap")
            .field("registered", &self.names)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Db(u32);
    #[derive(Debug, PartialEq)]
    struct Cache(&'static str);

    #[test]
    fn values_round_trip_by_type() {
        let mut map = StateMap::new();
        map.insert(Db(7));
        map.insert(Cache("redis"));
        assert_eq!(*map.get::<Db>().unwrap(), Db(7));
        assert_eq!(*map.get::<Cache>().unwrap(), Cache("redis"));
    }

    #[test]
    fn missing_types_are_none_not_a_panic() {
        assert!(StateMap::new().get::<Db>().is_none());
    }

    #[test]
    fn reinserting_the_same_type_replaces_and_reports_it() {
        let mut map = StateMap::new();
        assert!(!map.insert(Db(1)));
        assert!(map.insert(Db(2)));
        assert_eq!(*map.get::<Db>().unwrap(), Db(2));
        assert_eq!(map.registered().len(), 1);
    }
}
