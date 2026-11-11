// DRAFT append to src/kitty_graphics.rs under cfg(test), after source lock review.
// No mutation, drain, decode, elapsed-time call, or hashing.
#[cfg(test)]
impl KittyGraphicsState {
    pub(crate) fn title_oracle_snapshot(&self) -> serde_json::Value {
        let mut images = self.images.iter().collect::<Vec<_>>();
        images.sort_by_key(|(id, _)| **id);
        let mut pending = self.pending_placements.iter().collect::<Vec<_>>();
        pending.sort_by_key(|(id, _)| **id);
        // Assembler Debug is complete but contains an internal HashMap. Seeds
        // admit at most ONE pending id and title-only corpus admits no APC;
        // therefore order is deterministic in this lane. If the corpus expands
        // to multiple in-flight images this MUST be replaced by a typed sorted
        // snapshot in the shared protocol, not a fingerprint or regex rewrite.
        let assembler = format!("{:?}", self.assembler);
        // Inspected Pending's Debug contains only typed numbers/enums/Vec<u8>.
        // Count structural nodes, never rewrite content. Full source is sealed.
        let pending_nodes = assembler.matches("Pending {").count();
        assert!(
            pending_nodes <= 1,
            "single in-flight identity required for deterministic Debug map order"
        );
        assert_eq!(pending_nodes > 0, self.assembler.has_pending());
        assert!(pending.len() <= 1);
        if pending_nodes == 1 {
            assert_eq!(self.assembler.pending_bytes(), 4);
        }
        serde_json::json!({
            "images":format!("{images:?}"),
            "placements":format!("{:?}",self.placements),
            "assembler":assembler,
            "responses":self.responses,
            "pending_placements":format!("{pending:?}"),
            "chunked_target":format!("{:?}",self.chunked_target),
            "next_generation":self.next_generation,
            "total_decoded":self.total_decoded,
            "total_bytes_processed":self.total_bytes_processed,
            "total_image_memory":self.total_image_memory,
            "access_order":self.access_order,
        })
    }
}
