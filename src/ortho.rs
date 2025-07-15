#[derive(Debug)]
pub struct Ortho {
    version: u64,
}

impl Ortho {
    pub fn new(version: u64) -> Self {
        Ortho { version }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub(crate) fn get_required_and_forbidden(&self) -> (Vec<Vec<u16>>, Vec<u16>) {
        todo!()
    }

    pub(crate) fn add(&self, _to_add: u16, version: u64) -> Ortho {
        Ortho::new(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_stores_version() {
        let ortho = Ortho::new(42);
        assert_eq!(ortho.version(), 42);
    }

    #[test]
    fn test_version_returns_stored_value() {
        let ortho = Ortho::new(123);
        assert_eq!(ortho.version(), 123);
    }

    #[test]
    fn test_add_returns_ortho_with_new_version() {
        let ortho = Ortho::new(1);
        let new_ortho = ortho.add(42, 5);
        assert_eq!(new_ortho.version(), 5);
    }

    #[test]
    fn test_add_preserves_original_version() {
        let ortho = Ortho::new(10);
        let _new_ortho = ortho.add(42, 20);
        assert_eq!(ortho.version(), 10); // Original should be unchanged
    }

    #[test]
    fn test_version_comparison_with_interner() {
        // Test the logic used in processor.rs: cur.version() > interner.version()
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        let interner_version = 1;
        
        assert!(!(ortho_v1.version() > interner_version)); // 1 > 1 = false
        assert!(ortho_v2.version() > interner_version);    // 2 > 1 = true
    }
}
