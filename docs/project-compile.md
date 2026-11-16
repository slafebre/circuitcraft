# Project compile: what it takes

> **Written:** 2026-11-14 · Feeds the 0.3 and 0.5 milestones in [ROADMAP.md](../ROADMAP.md)

In the Altium Designer workflow, compiling a project turns a set of schematic sheets into one
design: every component with its pins and every net with what it connects. Everything after it
reads that design: the electrical rules check, the messages and navigation panels, the bill of
materials, cross-probing, and the update that carries the schematic onto the board. This page says
what compiling has to do, what CircuitCraft has today, and the order to build the rest in.

The behaviour described here comes from Altium's public documentation and general knowledge of
the workflow, in our own words. Details such as which net name wins a conflict are to be checked
by black-box observation once there is a reference install (an open decision in the roadmap).

PCBCraft (MIT OR Apache-2.0) works on the same problems for a KiCad-style editor. Its design
notes on [persistent net identity](https://github.com/tangency-labs/pcbcraft/blob/e6bddcb0700db7abe400f9dff679be66847322fd/docs/net-identity.md)
and [reviewed board updates](https://github.com/tangency-labs/pcbcraft/blob/e6bddcb0700db7abe400f9dff679be66847322fd/docs/single-board-update.md)
shaped decisions 4, 5 and 8 and step C10. We took ideas from them, written here in our own words,
and no code.

## What compiling does

1. **Reads the project.** The project lists its sheets and its board. Sheet symbols on a sheet
   name the child sheets they stand for, so the sheets form a tree with a top sheet.
2. **Finds connectivity on each sheet.** Wires join where they meet end to end, where a wire ends
   on another wire or a pin, and at junctions. Net labels name the wire they sit on. Power ports
   join every point with the same net name. Ports and off-sheet connectors leave the sheet. A bus
   carries numbered members (`D[0..7]`) that bus entries and labels pick out.
3. **Joins the sheets.** A sheet entry on a sheet symbol joins the port of the same name on the
   child sheet. Depending on the project's net identifier scope, labels and ports may also join
   across sheets by name alone (flat), only through sheet entries (hierarchical), or by a rule
   chosen from what the sheets use (automatic). Power ports join across the whole project.
4. **Names every net.** Where one net has several names, a priority picks one (power ports, then
   names from higher sheets, then labels) and the others are reported. A net without a name gets
   one made from a pin on it, the same one every time.
5. **Assembles components.** A component may be drawn as several parts (four gates of a quad
   NAND, on different sheets); compiling makes one component of them. Hidden power pins join the
   net they name. Each component keeps an identity that survives renaming, so the board can find
   it again after annotation changes its designator.
6. **Collects design intent.** Parameters on nets and components (net classes, differential pairs,
   rule values, footprint and model links, "not fitted") travel with the design to the board.
7. **Reports.** Every problem found becomes a message with a level and a place on a sheet. The
   electrical rules check adds its own: a pin-type connection matrix (an output driving an output,
   an input nobody drives), unconnected pins, nets with one pin, duplicate designators, labels on
   no wire, ports without a partner. No-ERC markers silence a check at one point.
8. **Feeds the board.** A comparator matches the compiled design against the board (components by
   identity, nets by what they connect) and lists the changes: add or remove a component, change
   its footprint, value or designator, add, rename or remove a net, move a pad to another net.
   The user reviews the list, applies some or all of it as one step, and can carry changes made on
   the board (designators, footprints, swapped pins) back to the schematic.

## Where CircuitCraft stands

| Area | Today | Gap |
|---|---|---|
| Schematic model | Nothing: `model` holds only a board | Sheets, components and their parts, pins with electrical types, wires, junctions, labels, power ports, ports, sheet symbols and entries, buses, directives, no-ERC markers |
| Schematic files | `.ccsch` is reserved: `file.zip` packs it, nothing reads or writes it | Reader and writer in `crates/io/src/native.rs`, deterministic, one item per line, fuzzed |
| Project file | `.ccproj` holds a name and one board | An ordered list of sheets, compile options (net identifier scope), the ERC settings and a copy of each symbol the sheets use |
| Session | Holds one board `Document` | Several documents (sheets and a board), each with undo, and commands that change two documents as one undo step (updating the board, back-annotation) |
| Identity | A footprint's id is documented as "shared with the schematic symbol once schematics exist" | A link that still works for multi-part components and repeated sheets (see decision 1) |
| Board nets | `Net { id, name }`; pads, tracks and vias carry a `NetId` | Nets need to survive an update that renames them, and later carry a class; the board needs to remember what the last update accepted (decision 8) |
| Imports | `Format` recognises EAGLE and KiCad schematics; `file.open` refuses them | Read-only schematic import, so compile has real input before the editor exists |
| Connectivity, compile, ERC, comparator | Nothing; the layer table has no crate for them | A new L3 crate (proposed name `netlist`, depending on `model`) |
| Libraries | The `library` crate is reserved, not created | Symbols for the editor; footprints the board update can place |
| UI | "Schematics arrive in 0.3" | Sheet view, Projects, Messages and Navigator panels, cross-probing |
| Corpus | Nine boards pinned by commit | Their schematics, pinned at the same commits, as the oracle for compile |

## The plan

Each step is one or a few pull requests with its own tests. The order puts a working compile in
front of agents and the CLI before the schematic editor, because imported schematics give compile
real input and the corpus boards give it a right answer: **compile each corpus schematic and
compare it with its board.** The same components should appear by designator, and each net should
join the same pads, whatever the nets are called. A schematic and board that disagree upstream are
recorded in the manifest, as import gaps are today.

| Step | Deliverable | Needs | Milestone | Size |
|---|---|---|---|---|
| C0 | **Several documents in a session.** Undo by change set first (already a 0.1 row), then a session holding sheets and a board, each with its own history, and one undo step for a command that changes more than one | — | 0.1 / start of 0.3 | M |
| C1 | **Schematic model** in `model` (a `schematic` module): the items listed above, with limits and validation as strict as the board's. A placed part takes its drawing and pins from the project's copy of its symbol (decision 4) | Decisions 1 and 4 | 0.3 | M |
| C2 | **`.ccsch` and `.ccproj` v1**: sheets in the project file with compile options and ERC settings; the project's symbol copies; schematic reader and writer; golden and fuzz tests | C1 | 0.3 | S |
| C3 | **KiCad schematic import** (`.kicad_sch`, the versions the board importer reads), hierarchical sheets included; corpus schematics pinned in the manifest | C1, decision 7 | 0.3 | M |
| C4 | **Connectivity on one sheet** in `netlist`: wires, junctions, pins, labels, power ports, ports, buses expanded into members; generated names that never change between runs. A pure function of the sheet, with property tests (moving a sheet, or saving and reading it, changes no net) | C1 | 0.3 | M |
| C5 | **Compile the project**: the sheet tree (depth bounded, cycles reported), net identifier scope, naming priority, multi-part components, hidden power pins, component identity, directives; a deterministic compiled design with messages. Commands `project.compile`, `netlist.query` and `net.info`; `circuitcraft-cli compile <project>` exits non-zero on errors, so a user's CI can run it. The corpus comparison above runs in `cargo xtask corpus` | C0, C4, decisions 2 and 3 | 0.3 | L |
| C6 | **Electrical rules check**: each check with a default level (off, warning, error, fatal); the pin-type connection matrix (input, output, bidirectional, passive, open collector, open emitter, high impedance, power input, power output, unspecified, plus ports and sheet entries), checked per net from the set of pin types on it rather than pin pair by pin pair, so a ground net with hundreds of pins stays cheap; no-ERC markers; settings saved in the project; `erc.run` gives a JSON report | C5 | 0.3 | M |
| C7 | **EAGLE schematic import** (`.sch`, the XML format), multi-gate parts and their supply pins included | C1, C5 | 0.3 | M |
| C8 | **Annotation**: `schematic.annotate` in a chosen order, keeping or resetting existing designators; duplicates reported by compile | C5 | 0.3 | S |
| C9 | **In the app**: sheet drawing in `render`, the Projects panel (the sheet tree), Messages (filter by level, click to go to the place) and Navigator (components, nets, pins of the compiled design); compile from the Project menu. Grows with the schematic editor row | C5, C6 | 0.3 | M |
| C10 | **Comparator and board update**: the board saves what the last applied update accepted (decision 8), and the comparator holds the compiled design and the board each against it, so every change says which side it came from. Components match by component key, falling back to the designator (decision 1). The change list is data (`eco.preview`) and comes with a token tied to the revisions it was made from; `eco.apply` takes the token, refuses it if either side has changed since, and applies all of the list or the chosen part as one undoable step. A component deleted from the schematic is listed for removal; its footprint stays on the board until the person applies that change. New footprints are placed beside the board, grouped by sheet; board nets keep their id by decision 5 | C5, decisions 5 and 8, footprint resolution from the 0.5 footprint library and generator | 0.5 | L |
| C11 | **Back-annotation and cross-probing**: designators, footprints and swapped pins carried from the board to the sheets, found as the board's side of the comparison against the accepted state; selecting on one selects on the other | C10 | 0.5 | M |
| C12 | **Directives to rules**: net classes, differential pairs and rule parameters from the schematic become board rules | C10, the `rules` crate | 0.5 | S |

C3, C4 and the start of C9 can run side by side once C1 lands. The symbol library format and
symbol editor (a 0.3 row of its own) are not needed by compile, because the project carries a copy
of every symbol it uses; they are needed to place new parts in the editor.

A useful product falls out early: once C3, C5 and the comparison exist, a KiCad user can check
that a schematic and its board agree, over the CLI or MCP, before CircuitCraft can edit either.

## Decisions needed

| # | Decision | Recommendation | Needed by |
|---|---|---|---|
| 1 | **How a footprint finds its component** (decided 2026-11-15). Sharing one id breaks as soon as one component has several parts (several symbol placements, one footprint) or a sheet is used twice (one symbol, several footprints) | The schematic component has an id of its own, apart from the ids of its placed parts. It belongs to the sheet that holds its first part; parts on other sheets name it by that sheet and the component id (how they find the right use when that sheet is used more than once is settled with repeated sheets). A footprint carries a component key: the path of sheet-symbol ids from the top sheet to the sheet the component belongs to, then the component id. A flat design has an empty path. The board format changes once, before repeated sheets need it. The path needs no records of its own, but it changes when a sheet moves under another parent or a sheet symbol is replaced; the comparator then falls back to the designator and lists the pair as a relink to review, not as a removal and an addition | C1 |
| 2 | **When compiling happens.** Altium compiles when asked and when another step needs it, so its results can be stale | A compiled design that is a pure function of the documents, cached by their revisions: always current when asked for, so an agent never reads a stale netlist, and the Messages panel can refresh as the person draws | C5 |
| 3 | **Net naming rules**: the default net identifier scope and which name wins | The scopes and priority described above, with the details checked by black-box observation before C5 lands | C5 |
| 4 | **Symbol copies** (decided 2026-11-15). A sheet can hold a copy of each symbol or only a reference to the library | The project holds one copy of each symbol it uses, with its library link (library, item, revision) and a checksum of its content; placed parts refer to that copy. A project compiles without its libraries, a changed symbol shows in a diff once, however many parts use it, and "update from library" is an explicit command. KiCad and EAGLE schematics already carry their symbols this way, so imports map onto it. A copy in every placed part would repeat a drawing a hundred times on a board of resistors and let the copies drift apart; footprints on the board stay one copy per placement, because board files arrive that way | C1 |
| 5 | **Net identity across an update** | Anchors decide, not a count of pads. The accepted state records up to four anchors for each net, in a fixed order: pins first (by component key and pin), because the board sees them too, then net labels and ports (by sheet path and id), the lowest first. At an update, the compiled net that holds a board net's anchors continues it: it keeps the id and class whatever its name becomes, and the comparator reports a rename, not a removal and an addition. If a net splits, the part that holds its first surviving anchor keeps the id and the others get new ones. If no compiled net holds any of its anchors, the net is listed as removed, and the person can link it to a new net in the review, which keeps its id and class; that choice is saved, never guessed again. A net with one pin and no name has no anchors, so joining it to another net is that net gaining a member, not a merge. A real merge gets a new id, with conflicting classes left for the person to choose. Applying an update drops the anchors that are gone and fills the list up to four again in the same order; edits between updates don't touch it. Counting pads would flip on an even split and move a class to the wrong side without a word | C10 |
| 6 | **Netlist output formats** | Our own JSON from `project.compile` first. IPC-D-356 (public specification, for bare-board test) later, with the fabrication outputs | 0.5 |
| 7 | **Corpus schematics** | Pin each corpus board's schematic files at the board's commit, under the same licence; add a schematic-only design or two with hierarchical sheets and buses | C3 |
| 8 | **What the comparator compares against.** Comparing the compiled design with the board alone can't tell a change made on the schematic from one made on the board, so it can't say which way a change should go, and a footprint whose component was deleted would be reported on every comparison | The board file saves the accepted state: each component's key, designator, value, pins and footprint, and each net's pins and anchors (decision 5), as of the last update applied. A change on the schematic is a difference between the compiled design and the accepted state, a change on the board one between the board and the accepted state, and a conflict both sides changing the same thing differently; both making the same change is not a conflict. Applying a change in either direction, an update or a back-annotation, moves the accepted state to it; a change the person declines leaves it as it was, so it is listed again next time. A board opened without an accepted state (an import, a board laid out before its schematic existed) adopts one through an explicit command before its first update: built from the compiled design, with footprints matched by designator and nets by the pads they share, and whatever doesn't match listed. Bounded like every other part of the board file | C10 |
