//! The contract table (RFC 0006 phase 4): optional component `contracts`,
//! postcard `Vec<ContractRow>` in the order of `ResolvedGraph::contracts`.
//! Written only when the graph has rows; a segment without the component
//! (older, or no contracts) has an empty table.

use rueca_model::{ContractRow, NodeId, ResolvedGraph};

use crate::bundle::{Bundle, BundleWriter};
use crate::{Result, SegmentError};

/// Bundle component name of the contract table.
pub(crate) const CONTRACTS_COMPONENT: &str = "contracts";

/// Writes the contract table of `graph`, if it has rows.
pub(crate) fn write_contracts(w: &mut BundleWriter, graph: &ResolvedGraph) -> Result<()> {
    if graph.contracts.is_empty() {
        return Ok(());
    }
    let bytes = postcard::to_stdvec(&graph.contracts).map_err(|e| SegmentError::Other(e.to_string()))?;
    w.add(CONTRACTS_COMPONENT, &bytes)
}

/// The decoded contract table, checked against the node and file tables.
#[derive(Debug, Default)]
pub(crate) struct ContractTable {
    pub rows: Vec<ContractRow>,
    /// Row indexes sorted by (node, row).
    by_node: Vec<u32>,
}

impl ContractTable {
    /// Decodes and validates the component; empty when absent.
    pub fn open(bundle: &Bundle, nodes: usize, files: usize) -> Result<ContractTable> {
        let Ok(bytes) = bundle.bytes(CONTRACTS_COMPONENT) else { return Ok(ContractTable::default()) };
        let rows: Vec<ContractRow> =
            postcard::from_bytes(bytes).map_err(|e| SegmentError::Corrupt(format!("{CONTRACTS_COMPONENT}: {e}")))?;
        if rows.iter().any(|r| r.node as usize >= nodes || r.file as usize >= files) {
            return Err(SegmentError::Corrupt(format!("{CONTRACTS_COMPONENT}: node or file out of range")));
        }
        let mut by_node: Vec<u32> = (0..rows.len() as u32).collect();
        by_node.sort_by_key(|&i| (rows[i as usize].node, i));
        Ok(ContractTable { rows, by_node })
    }

    /// Rows placed on `node`, in table order.
    pub fn of(&self, node: NodeId) -> Vec<&ContractRow> {
        let lo = self.by_node.partition_point(|&i| self.rows[i as usize].node < node);
        let hi = lo + self.by_node[lo..].partition_point(|&i| self.rows[i as usize].node <= node);
        self.by_node[lo..hi].iter().map(|&i| &self.rows[i as usize]).collect()
    }
}
