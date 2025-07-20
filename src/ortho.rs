pub struct Ortho {
    version: usize,
    dims: Vec<usize>,
    payload: Vec<usize>,
}

impl Ortho {
    pub fn new(version: usize) -> Self {
        Ortho {
            version,
            dims: vec![2, 2],
            payload: vec![],
        }
    }

    pub fn add(&self, value: usize) -> Self {
        let mut payload = self.payload.clone();
        payload.push(value);
        let end = std::cmp::min(self.dims.len() + 1, payload.len());
        payload[1..end].sort(); // note that there may be a faster way to sort this as it is mostly sorted already
        Ortho {
            version: self.version,
            dims: self.dims.clone(),
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::result;

    use super::*;
    #[test]
    fn test_new() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.version, 1);
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![]);
    }
}
