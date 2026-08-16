//! A deterministic dependency graph for cell calculation.
//!
//! An edge points from a dependent cell to a cell it reads. This direction
//! matches formulas directly (`total -> price`) while the reverse index makes
//! invalidation inexpensive (`price -> total`). Both indices are updated as a
//! single logical operation and contain an entry for every known node,
//! including cells with no edges.
//!
//! The graph deliberately has no formula-parser dependency. Formula lowering
//! resolves references into [`CellKey`] values, then supplies those values to
//! [`DependencyGraph::set_dependencies`]. Keeping that boundary small lets the
//! evaluator and alternate formula frontends share the same invalidation and
//! cycle semantics.

use std::collections::{BTreeMap, BTreeSet};

use marksheet_model::{Coordinate, SheetId};
use serde::{Deserialize, Serialize};

/// The identity of one cell in a workbook.
///
/// Coordinates are not unique across sheets, so calculation state must always
/// use this compound key rather than a bare A1 coordinate.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CellKey {
    /// The workbook-stable identifier of the containing sheet.
    pub sheet: SheetId,
    /// The one-based coordinate within [`Self::sheet`].
    pub coordinate: Coordinate,
}

impl CellKey {
    /// Creates a workbook-qualified cell key.
    #[must_use]
    pub fn new(sheet: SheetId, coordinate: Coordinate) -> Self {
        Self { sheet, coordinate }
    }
}

impl std::fmt::Display for CellKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}!{}", self.sheet, self.coordinate)
    }
}

/// One deterministic calculation step.
///
/// A [`Self::Cycle`] step contains exactly one strongly connected component.
/// It must be assigned circular-reference results before later steps consume
/// it. A singleton is emitted as a cycle only when it directly depends on
/// itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EvaluationStep {
    /// A non-cyclic cell whose cross-component dependencies precede it.
    Cell(CellKey),
    /// A circular-reference component, ordered by [`CellKey`].
    Cycle(BTreeSet<CellKey>),
}

/// Cheap, cache-friendly facts about a [`DependencyGraph`] snapshot.
///
/// `revision` changes whenever the graph's known-node or edge set changes.
/// Consumers may use it as a cache key, while treating its eventual `u64`
/// wraparound like any other generation counter rollover.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphStats {
    /// Monotonically wrapping mutation generation.
    pub revision: u64,
    /// Number of known cells, including isolated cells.
    pub nodes: usize,
    /// Number of directed formula-reference edges.
    pub edges: usize,
    /// Number of strongly connected components that are circular.
    pub cyclic_components: usize,
    /// Number of cells participating directly in a circular component.
    pub cyclic_cells: usize,
}

/// Directed formula dependencies with a maintained reverse index.
///
/// Invariants:
///
/// * `forward[a]` contains `b` if and only if `reverse[b]` contains `a`.
/// * Every key or edge endpoint exists in both maps, even if it has no edges.
/// * `BTreeMap` and `BTreeSet` make externally observable traversal stable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyGraph {
    /// `formula_cell -> cells read by that formula`.
    forward: BTreeMap<CellKey, BTreeSet<CellKey>>,
    /// `changed_cell -> formulas that read that cell`.
    reverse: BTreeMap<CellKey, BTreeSet<CellKey>>,
    revision: u64,
}

impl DependencyGraph {
    /// Creates an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the mutation generation suitable for cache invalidation.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns `true` when no cells have been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// Returns the number of known cells.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.forward.len()
    }

    /// Returns the number of directed dependencies.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.forward.values().map(BTreeSet::len).sum()
    }

    /// Returns whether `cell` is a known node.
    #[must_use]
    pub fn contains_cell(&self, cell: &CellKey) -> bool {
        self.forward.contains_key(cell)
    }

    /// Registers `cell` as an isolated node when it is not already known.
    ///
    /// Returns whether the graph changed.
    pub fn ensure_cell(&mut self, cell: CellKey) -> bool {
        let changed = self.ensure_cell_internal(cell);
        if changed {
            self.bump_revision();
        }
        debug_assert!(self.indices_are_consistent());
        changed
    }

    /// Removes `cell` and every edge entering or leaving it.
    ///
    /// A formula that had read the removed cell remains known, but no longer
    /// has that graph edge. Reference validity belongs to formula resolution,
    /// not to this structural graph.
    pub fn remove_cell(&mut self, cell: &CellKey) -> bool {
        let Some(outgoing) = self.forward.remove(cell) else {
            return false;
        };
        let incoming = self.reverse.remove(cell).unwrap_or_default();

        for dependency in outgoing {
            if let Some(dependents) = self.reverse.get_mut(&dependency) {
                dependents.remove(cell);
            }
        }
        for dependent in incoming {
            if let Some(dependencies) = self.forward.get_mut(&dependent) {
                dependencies.remove(cell);
            }
        }

        self.bump_revision();
        debug_assert!(self.indices_are_consistent());
        true
    }

    /// Replaces all dependencies read by `cell`.
    ///
    /// Supplying an empty iterator records `cell` as a formula-free or literal
    /// node. Duplicate inputs are folded into one edge. Both the dependent and
    /// every referenced cell become known graph nodes.
    pub fn set_dependencies<I>(&mut self, cell: CellKey, dependencies: I) -> bool
    where
        I: IntoIterator<Item = CellKey>,
    {
        let desired: BTreeSet<_> = dependencies.into_iter().collect();
        let previous = self.forward.get(&cell).cloned().unwrap_or_default();
        let mut changed = previous != desired;
        changed |= self.ensure_cell_internal(cell.clone());

        for dependency in &desired {
            changed |= self.ensure_cell_internal(dependency.clone());
        }

        for removed in previous.difference(&desired) {
            if let Some(dependents) = self.reverse.get_mut(removed) {
                dependents.remove(&cell);
            }
        }
        for added in desired.difference(&previous) {
            if let Some(dependents) = self.reverse.get_mut(added) {
                dependents.insert(cell.clone());
            }
        }
        self.forward.insert(cell, desired);

        if changed {
            self.bump_revision();
        }
        debug_assert!(self.indices_are_consistent());
        changed
    }

    /// Adds one edge from `dependent` to `dependency`.
    ///
    /// Returns whether a node or edge was added.
    pub fn add_dependency(&mut self, dependent: CellKey, dependency: CellKey) -> bool {
        let mut changed = self.ensure_cell_internal(dependent.clone());
        changed |= self.ensure_cell_internal(dependency.clone());
        if let Some(dependencies) = self.forward.get_mut(&dependent) {
            changed |= dependencies.insert(dependency.clone());
        }
        if changed {
            self.reverse
                .entry(dependency)
                .or_default()
                .insert(dependent);
            self.bump_revision();
        }
        debug_assert!(self.indices_are_consistent());
        changed
    }

    /// Removes the edge from `dependent` to `dependency`, retaining both nodes.
    ///
    /// Returns whether an edge was removed.
    pub fn remove_dependency(&mut self, dependent: &CellKey, dependency: &CellKey) -> bool {
        let removed = self
            .forward
            .get_mut(dependent)
            .is_some_and(|dependencies| dependencies.remove(dependency));
        if removed {
            if let Some(dependents) = self.reverse.get_mut(dependency) {
                dependents.remove(dependent);
            }
            self.bump_revision();
        }
        debug_assert!(self.indices_are_consistent());
        removed
    }

    /// Removes every edge read by `cell`, retaining `cell` as a known node.
    ///
    /// Returns whether at least one edge was removed.
    pub fn clear_dependencies(&mut self, cell: &CellKey) -> bool {
        let Some(previous) = self.forward.get(cell).cloned() else {
            return false;
        };
        if previous.is_empty() {
            return false;
        }
        for dependency in &previous {
            if let Some(dependents) = self.reverse.get_mut(dependency) {
                dependents.remove(cell);
            }
        }
        self.forward.insert(cell.clone(), BTreeSet::new());
        self.bump_revision();
        debug_assert!(self.indices_are_consistent());
        true
    }

    /// Iterates all known cells in stable key order.
    pub fn cells(&self) -> impl Iterator<Item = &CellKey> {
        self.forward.keys()
    }

    /// Iterates cells read by `cell` in stable key order.
    #[must_use]
    pub fn dependencies_of(&self, cell: &CellKey) -> impl DoubleEndedIterator<Item = &CellKey> {
        self.forward.get(cell).into_iter().flatten()
    }

    /// Iterates formulas that directly read `cell`, in stable key order.
    #[must_use]
    pub fn dependents_of(&self, cell: &CellKey) -> impl DoubleEndedIterator<Item = &CellKey> {
        self.reverse.get(cell).into_iter().flatten()
    }

    /// Returns all cells invalidated by changing `roots`, including `roots`.
    ///
    /// Unknown roots are retained in the result. This is important when a
    /// caller records a literal value before its formula node has been added.
    /// Traversal follows reverse edges and deduplicates diamond-shaped paths.
    #[must_use]
    pub fn dirty_closure<I>(&self, roots: I) -> BTreeSet<CellKey>
    where
        I: IntoIterator<Item = CellKey>,
    {
        let mut dirty: BTreeSet<_> = roots.into_iter().collect();
        let mut pending = dirty.clone();

        while let Some(next) = pending.pop_first() {
            for dependent in self.dependents_of(&next) {
                if dirty.insert(dependent.clone()) {
                    pending.insert(dependent.clone());
                }
            }
        }
        dirty
    }

    /// Returns all cells invalidated by changing one `root`, including it.
    #[must_use]
    pub fn dirty_from(&self, root: CellKey) -> BTreeSet<CellKey> {
        self.dirty_closure([root])
    }

    /// Finds strongly connected components with deterministic Kosaraju DFS.
    ///
    /// Components and the cells within them are both key-sorted. A singleton
    /// component is reported even when it is not a cycle; use
    /// [`Self::cyclic_components`] when only circular references are needed.
    #[must_use]
    pub fn strongly_connected_components(&self) -> Vec<BTreeSet<CellKey>> {
        let mut visited = BTreeSet::new();
        let mut finish_order = Vec::with_capacity(self.node_count());

        for start in self.cells() {
            if !visited.insert(start.clone()) {
                continue;
            }
            // A frame retains its next-neighbor position. Marking every
            // sibling before either one is explored is subtly wrong: a later
            // sibling can be reachable through the earlier sibling, which
            // changes DFS finish order and can corrupt Kosaraju's SCC pass.
            let mut stack = vec![(
                start.clone(),
                self.dependencies_of(start).cloned().collect::<Vec<_>>(),
                0_usize,
            )];
            while let Some((_, dependencies, next_dependency)) = stack.last_mut() {
                if let Some(dependency) = dependencies.get(*next_dependency).cloned() {
                    *next_dependency += 1;
                    if visited.insert(dependency.clone()) {
                        stack.push((
                            dependency.clone(),
                            self.dependencies_of(&dependency).cloned().collect(),
                            0,
                        ));
                    }
                } else if let Some((finished, _, _)) = stack.pop() {
                    finish_order.push(finished);
                }
            }
        }

        let mut assigned = BTreeSet::new();
        let mut components = Vec::new();
        for start in finish_order.into_iter().rev() {
            if !assigned.insert(start.clone()) {
                continue;
            }
            let mut component = BTreeSet::new();
            let mut stack = vec![start];
            while let Some(cell) = stack.pop() {
                component.insert(cell.clone());
                for dependent in self.dependents_of(&cell).rev() {
                    if assigned.insert(dependent.clone()) {
                        stack.push(dependent.clone());
                    }
                }
            }
            components.push(component);
        }

        components.sort_by_key(|component| component.first().cloned());
        components
    }

    /// Returns exactly the strongly connected components that are cycles.
    ///
    /// A self-reference is a cycle, so singleton components are retained only
    /// if their cell has an edge to itself.
    #[must_use]
    pub fn cyclic_components(&self) -> Vec<BTreeSet<CellKey>> {
        self.strongly_connected_components()
            .into_iter()
            .filter(|component| self.component_is_cyclic(component))
            .collect()
    }

    /// Returns a stable calculation sequence over the condensation DAG.
    ///
    /// Every referenced cell appears before its dependent unless both belong
    /// to the same circular component. Those circular components are emitted as
    /// one [`EvaluationStep::Cycle`] so an evaluator can set `#CIRC!` before
    /// evaluating later consumers of that error.
    #[must_use]
    pub fn evaluation_order(&self) -> Vec<EvaluationStep> {
        let components = self.strongly_connected_components();
        let mut component_for_cell = BTreeMap::new();
        for (index, component) in components.iter().enumerate() {
            for cell in component {
                component_for_cell.insert(cell.clone(), index);
            }
        }

        // The forward graph is dependent -> dependency. Condensation edges are
        // reversed here so Kahn's algorithm emits dependencies first.
        let mut pending_dependencies = vec![BTreeSet::new(); components.len()];
        let mut component_dependents = vec![BTreeSet::new(); components.len()];
        for (dependent, dependencies) in &self.forward {
            let dependent_component = component_for_cell[dependent];
            for dependency in dependencies {
                let dependency_component = component_for_cell[dependency];
                if dependent_component != dependency_component
                    && pending_dependencies[dependent_component].insert(dependency_component)
                {
                    component_dependents[dependency_component].insert(dependent_component);
                }
            }
        }

        let mut ready = BTreeSet::new();
        for (index, dependencies) in pending_dependencies.iter().enumerate() {
            if dependencies.is_empty() {
                if let Some(first) = components[index].first() {
                    ready.insert((first.clone(), index));
                }
            }
        }

        let mut order = Vec::with_capacity(components.len());
        while let Some((_, component_index)) = ready.pop_first() {
            let component = &components[component_index];
            if self.component_is_cyclic(component) {
                order.push(EvaluationStep::Cycle(component.clone()));
            } else if let Some(cell) = component.first() {
                order.push(EvaluationStep::Cell(cell.clone()));
            }

            for dependent_component in component_dependents[component_index].clone() {
                let removed = pending_dependencies[dependent_component].remove(&component_index);
                debug_assert!(removed, "condensation indexes stay synchronized");
                if pending_dependencies[dependent_component].is_empty() {
                    if let Some(first) = components[dependent_component].first() {
                        ready.insert((first.clone(), dependent_component));
                    }
                }
            }
        }
        debug_assert_eq!(
            order.len(),
            components.len(),
            "the condensation graph is acyclic"
        );
        order
    }

    /// Returns current counts and circular-reference facts in one traversal.
    #[must_use]
    pub fn stats(&self) -> GraphStats {
        let cyclic_components = self.cyclic_components();
        GraphStats {
            revision: self.revision,
            nodes: self.node_count(),
            edges: self.edge_count(),
            cyclic_cells: cyclic_components.iter().map(BTreeSet::len).sum(),
            cyclic_components: cyclic_components.len(),
        }
    }

    fn ensure_cell_internal(&mut self, cell: CellKey) -> bool {
        let new_forward = !self.forward.contains_key(&cell);
        self.forward.entry(cell.clone()).or_default();
        let new_reverse = !self.reverse.contains_key(&cell);
        self.reverse.entry(cell).or_default();
        new_forward || new_reverse
    }

    fn component_is_cyclic(&self, component: &BTreeSet<CellKey>) -> bool {
        component.len() > 1
            || component.first().is_some_and(|cell| {
                self.forward
                    .get(cell)
                    .is_some_and(|dependencies| dependencies.contains(cell))
            })
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn indices_are_consistent(&self) -> bool {
        self.forward.len() == self.reverse.len()
            && self.forward.iter().all(|(dependent, dependencies)| {
                self.reverse.contains_key(dependent)
                    && dependencies.iter().all(|dependency| {
                        self.reverse
                            .get(dependency)
                            .is_some_and(|dependents| dependents.contains(dependent))
                    })
            })
            && self.reverse.iter().all(|(dependency, dependents)| {
                self.forward.contains_key(dependency)
                    && dependents.iter().all(|dependent| {
                        self.forward
                            .get(dependent)
                            .is_some_and(|dependencies| dependencies.contains(dependency))
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{CellKey, DependencyGraph, EvaluationStep};
    use marksheet_model::{Coordinate, SheetId};

    fn cell(sheet: &str, coordinate: &str) -> CellKey {
        CellKey::new(
            SheetId::parse(sheet).expect("valid test sheet"),
            Coordinate::parse(coordinate).expect("valid test coordinate"),
        )
    }

    fn cells(values: &[(&str, &str)]) -> Vec<CellKey> {
        values
            .iter()
            .map(|(sheet, coordinate)| cell(sheet, coordinate))
            .collect()
    }

    #[test]
    fn chain_orders_dependencies_before_dependents_and_propagates_dirtiness() {
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(b1.clone(), [a1.clone()]);
        graph.set_dependencies(c1.clone(), [b1.clone()]);

        assert_eq!(
            graph.dirty_from(a1.clone()),
            [a1.clone(), b1.clone(), c1.clone()].into()
        );
        assert_eq!(
            graph.evaluation_order(),
            vec![
                EvaluationStep::Cell(a1),
                EvaluationStep::Cell(b1),
                EvaluationStep::Cell(c1),
            ]
        );
        assert!(graph.cyclic_components().is_empty());
    }

    #[test]
    fn diamond_deduplicates_dirty_paths_in_stable_order() {
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let d1 = cell("main", "D1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(b1.clone(), [a1.clone()]);
        graph.set_dependencies(c1.clone(), [a1.clone()]);
        graph.set_dependencies(d1.clone(), [b1.clone(), c1.clone()]);

        assert_eq!(
            graph.dirty_from(a1),
            [b1, c1, d1]
                .into_iter()
                .chain([cell("main", "A1")])
                .collect()
        );
        let order = graph.evaluation_order();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], EvaluationStep::Cell(cell("main", "A1")));
        assert_eq!(order[3], EvaluationStep::Cell(cell("main", "D1")));
    }

    #[test]
    fn cross_sheet_dependencies_use_full_cell_identity() {
        let source = cell("inputs", "A1");
        let formula = cell("report", "A1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(formula.clone(), [source.clone()]);

        assert_eq!(
            graph.dependencies_of(&formula).cloned().collect::<Vec<_>>(),
            vec![source.clone()]
        );
        assert_eq!(
            graph.dependents_of(&source).cloned().collect::<Vec<_>>(),
            vec![formula.clone()]
        );
        assert_eq!(
            graph.dirty_from(source),
            [formula, cell("inputs", "A1")].into()
        );
    }

    #[test]
    fn two_cell_cycle_is_reported_and_its_downstream_remains_after_the_cycle() {
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(a1.clone(), [b1.clone()]);
        graph.set_dependencies(b1.clone(), [a1.clone()]);
        graph.set_dependencies(c1.clone(), [b1.clone()]);

        assert_eq!(
            graph.cyclic_components(),
            vec![[a1.clone(), b1.clone()].into()]
        );
        assert_eq!(
            graph.evaluation_order(),
            vec![
                EvaluationStep::Cycle([a1.clone(), b1].into()),
                EvaluationStep::Cell(c1),
            ]
        );
        assert_eq!(
            graph.dirty_from(a1.clone()),
            [a1, cell("main", "B1"), cell("main", "C1")].into()
        );
    }

    #[test]
    fn self_loop_is_a_cycle() {
        let a1 = cell("main", "A1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(a1.clone(), [a1.clone()]);

        assert_eq!(graph.cyclic_components(), vec![[a1.clone()].into()]);
        assert_eq!(
            graph.evaluation_order(),
            vec![EvaluationStep::Cycle([a1].into())]
        );
    }

    #[test]
    fn shared_descendant_dag_has_only_singleton_components() {
        // This shape is the critical iterative-DFS regression: discovering C
        // as A's sibling before B explores B -> C produces an invalid finish
        // order and can make Kosaraju merge all three cells into one SCC.
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(a1.clone(), [b1.clone(), c1.clone()]);
        graph.set_dependencies(b1.clone(), [c1.clone()]);
        graph.ensure_cell(c1.clone());

        assert_eq!(
            graph.strongly_connected_components(),
            vec![
                [a1.clone()].into(),
                [b1.clone()].into(),
                [c1.clone()].into(),
            ]
        );
        assert!(graph.cyclic_components().is_empty());
        assert_eq!(
            graph.evaluation_order(),
            vec![
                EvaluationStep::Cell(c1),
                EvaluationStep::Cell(b1),
                EvaluationStep::Cell(a1),
            ]
        );
    }

    #[test]
    fn replacing_edges_updates_both_indexes_and_revisions_only_on_change() {
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let mut graph = DependencyGraph::new();

        assert!(graph.set_dependencies(c1.clone(), [a1.clone()]));
        let revision = graph.revision();
        assert!(!graph.set_dependencies(c1.clone(), [a1.clone()]));
        assert_eq!(graph.revision(), revision);
        assert!(graph.set_dependencies(c1.clone(), [b1.clone()]));
        assert!(graph.dependents_of(&a1).next().is_none());
        assert_eq!(
            graph.dependents_of(&b1).cloned().collect::<Vec<_>>(),
            vec![c1.clone()]
        );
        assert_eq!(
            graph.dependencies_of(&c1).cloned().collect::<Vec<_>>(),
            vec![b1]
        );
        assert_eq!(graph.stats().edges, 1);
    }

    #[test]
    fn removing_a_cell_removes_all_incident_edges_without_removing_neighbors() {
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(b1.clone(), [a1.clone()]);
        graph.set_dependencies(c1.clone(), [b1.clone()]);

        assert!(graph.remove_cell(&b1));
        assert!(!graph.contains_cell(&b1));
        assert!(graph.contains_cell(&a1));
        assert!(graph.contains_cell(&c1));
        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.dirty_from(a1), [cell("main", "A1")].into());
    }

    #[test]
    fn multiple_roots_and_unknown_roots_have_an_exact_dirty_set() {
        let a1 = cell("main", "A1");
        let b1 = cell("main", "B1");
        let c1 = cell("main", "C1");
        let missing = cell("main", "Z99");
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(c1.clone(), [a1.clone(), b1.clone()]);

        let dirty = graph.dirty_closure([missing.clone(), a1.clone(), b1.clone()]);
        assert_eq!(dirty, [a1, b1, c1, missing].into());
    }

    #[test]
    fn stats_describe_cycles_without_counting_downstream_cells_as_circular() {
        let (a1, b1, c1) = {
            let values = cells(&[("main", "A1"), ("main", "B1"), ("main", "C1")]);
            (values[0].clone(), values[1].clone(), values[2].clone())
        };
        let mut graph = DependencyGraph::new();
        graph.set_dependencies(a1.clone(), [b1.clone()]);
        graph.set_dependencies(b1.clone(), [a1]);
        graph.set_dependencies(c1, [b1]);

        assert_eq!(
            graph.stats(),
            super::GraphStats {
                revision: 3,
                nodes: 3,
                edges: 3,
                cyclic_components: 1,
                cyclic_cells: 2,
            }
        );
    }
}
