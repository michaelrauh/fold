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

    pub(crate) fn add(&self, _to_add: u16, _version: u64) -> Ortho {
        todo!()
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
}
