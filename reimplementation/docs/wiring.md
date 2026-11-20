# Wiring editor subset

Wire mode connects explicitly selected component pins; crossings and overlaps do not create junctions. Select Wire or press W with canvas focus, then click two pins. Tab/Shift-Tab cycles pins and Enter chooses one; Escape cancels a pending connection. In normal mode Tab cycles wires and Delete removes the selected wire. Wire edits use the same undo/redo history as components.

This is not Digital's geometric wire model: no free ends, junction insertion, bus semantics, electrical validation, simulation, or .dig compatibility is provided. The behavior comparison uses Digital source revision d118dce079b6b8867024d40f9b14dfa667e52106; it is not a parity claim.
