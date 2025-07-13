use crate::interner::Interner;

pub struct Worker;

impl Worker {
    pub fn process(
        ortho: crate::ortho::Ortho,
        interner: &mut Interner,
    ) -> Vec<crate::ortho::Ortho> {
        // Version check moved to caller

        let (required, forbidden) = ortho.get_required_and_forbidden();
        let required_bits = interner.get_required_bits(&required);
        let forbidden_bits = interner.get_forbidden_bits(&forbidden);

        let results = interner.intersect(required_bits, forbidden_bits);

        results
            .iter()
            .map(|&result| ortho.add(result, interner.version()))
            .collect()
    }
}
