use std::collections::{BTreeMap, BTreeSet};

use crate::parse::{Spot, Spots};
use crate::{File, ITEMS_MAX, ParseError};

/// Checks a parsed file as a whole, each rule in source order: item names unique, every
/// named type declared as an enum or type, and no type that contains itself.
/// Inputs: the file and its spots. Output: nothing, or the first refusal.
pub(crate) fn check(file: &File, spots: &Spots) -> Result<(), ParseError> {
    assert_eq!(spots.fields_of_type.len(), file.types.len());
    let mut seen = BTreeSet::new();
    for spot in &spots.declared {
        if !seen.insert(spot.name.as_str()) {
            return Err(spot.refuse("cap_duplicate_name"));
        }
    }
    let types: BTreeMap<&str, usize> = file
        .types
        .iter()
        .enumerate()
        .map(|(index, ty)| (ty.name.as_str(), index))
        .collect();
    let enums: BTreeSet<&str> = file.enums.iter().map(|e| e.name.as_str()).collect();
    if let Some(unknown) = spots
        .used
        .iter()
        .find(|spot| !types.contains_key(spot.name.as_str()) && !enums.contains(spot.name.as_str()))
    {
        return Err(unknown.refuse("cap_unknown_type"));
    }
    acyclic(&types, &spots.fields_of_type)
}

/// Walks the types depth first, in source order, and refuses the first field whose type
/// is still on the walk's path. Inputs: type names to their index and each type's named
/// field types. Output: nothing, or `cap_type_cycle` at the field that closes the cycle.
fn acyclic(types: &BTreeMap<&str, usize>, fields_of_type: &[Vec<Spot>]) -> Result<(), ParseError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        New,
        OnPath,
        Done,
    }
    assert!(fields_of_type.len() <= ITEMS_MAX);
    let mut marks = vec![Mark::New; fields_of_type.len()];
    for root in 0..fields_of_type.len() {
        if marks[root] != Mark::New {
            continue;
        }
        marks[root] = Mark::OnPath;
        let mut path = vec![(root, 0usize)];
        while let Some(&(ty, next)) = path.last() {
            assert!(path.len() <= fields_of_type.len());
            let Some(spot) = fields_of_type[ty].get(next) else {
                marks[ty] = Mark::Done;
                path.pop();
                continue;
            };
            path.last_mut().expect("the path is not empty").1 += 1;
            let Some(&target) = types.get(spot.name.as_str()) else {
                continue;
            };
            match marks[target] {
                Mark::OnPath => return Err(spot.refuse("cap_type_cycle")),
                Mark::New => {
                    marks[target] = Mark::OnPath;
                    path.push((target, 0));
                }
                Mark::Done => {}
            }
        }
    }
    assert!(marks.iter().all(|mark| *mark == Mark::Done));
    Ok(())
}
