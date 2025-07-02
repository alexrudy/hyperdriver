use std::fmt;
use std::sync::Arc;

use std::sync::Weak;

use crate::DebugLiteral;

pub(crate) struct WeakOpt<T>(Option<Weak<T>>);

impl<T> WeakOpt<T> {
    pub(crate) fn none() -> Self {
        Self(None)
    }

    pub(crate) fn downgrade(arc: &Arc<T>) -> Self {
        Self(Some(Arc::downgrade(arc)))
    }

    pub(crate) fn upgrade(&self) -> Option<Arc<T>> {
        self.0.as_ref().and_then(|weak| weak.upgrade())
    }

    #[allow(dead_code)]
    pub(crate) fn is_none(&self) -> bool {
        self.0.is_none()
    }
}

impl<T> Clone for WeakOpt<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> fmt::Debug for WeakOpt<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(_) => f
                .debug_tuple("WeakOpt")
                .field(&DebugLiteral("Some(...)"))
                .finish(),
            None => f
                .debug_tuple("WeakOpt")
                .field(&DebugLiteral("None"))
                .finish(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_weak_opt {
    use super::*;

    #[test]
    fn weak_opt() {
        let arc = Arc::new(());
        let weak = WeakOpt::downgrade(&arc);
        assert!(weak.upgrade().is_some());
        drop(arc);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn weak_opt_none() {
        let weak = WeakOpt::<()>::none();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn weak_opt_debug() {
        let arc = Arc::new(());
        let weak = WeakOpt::downgrade(&arc);
        assert_eq!(format!("{weak:?}"), "WeakOpt(Some(...))");

        let weak: WeakOpt<()> = WeakOpt::none();
        assert_eq!(format!("{weak:?}"), "WeakOpt(None)");
    }
}
