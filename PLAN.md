# Plan: Art-Net-Visualizer mit MVR/GDTF (Rust)

Stand: 2026-10-31 · Status: Phase 0, 1 und 3 umgesetzt; Phase 2 als Nächstes

Der Rust-Port in [rust/](rust/) wird zum echten Visualizer: MVR laden, GDTF auswerten, Art-Net empfangen, DMX → Attribute → Bewegungsmodell → bestehender Volumetrik-Renderer. Standard-Teile über Libraries, eigen nur, wo es nichts gibt. Demo bleibt als `--demo`.

## Entscheidungen

- Basis: Rust-Port; Toolchain auf aktuelles Rust (`rustup default stable`, ≥ 1.85) → Pin `unicode-segmentation` entfällt, `clap`/`image` zurück
- Protokoll: Art-Net ArtDmx + ArtPoll/ArtPollReply (kein sACN)
- Sender: Pult auf demselben PC **und** im Netzwerk; ChamSys MagicQ, grandMA, Eigenentwicklung
- MVR: Fixtures + echte 3D-Modelle (3DS + glTF/GLB) für Szene und Traversen
- GDTF Phase 1: Pan/Tilt (16 Bit), Dimmer, Shutter/Strobe, RGB(W)/CMY/CTO, Farbrad, Gobo-Räder inkl. Index/Rotation mit GDTF-Gobos, Prisma inkl. Rotation, Zoom/Focus/Iris/Frost; **ohne** LED-Multicell
- Bewegungsmodell mit `RealFade`/`RealAcceleration` + Defaults
- Bedienung: freie Kamera (eigen, klein) + egui-Panel
- Testdaten in `testdata/` (MVR/GDTF aus grandMA/MagicQ)

## Libraries

| Teil | Library | Eigener Anteil |
|---|---|---|
| Art-Net-Pakete | `artnet_protocol` + `socket2` (Port teilen, Broadcast) | Empfangs-Thread, Puffer, PollReply-Inhalt |
| GDTF lesen | `gdtf` (Objektmodell + Ressourcen) | Auswertung für Renderer |
| DMX → Attribut | `zeevonk` (early dev, erst nach Spike) | sonst eigener Resolver |
| MVR | – | eigener Parser auf `zip` + `roxmltree` |
| 3D-Modelle | `ds3` (3DS), `gltf` (GLB) | – |
| UI | `egui` + `egui-winit` + `egui_glow` | Panels |
| Sonstiges | `image`, `clap` | – |
| Kamera, Bewegungsmodell, Volumetrik | – | eigen |

## Spec-Fakten

- **MVR:** zip mit `GeneralSceneDescription.xml`; Matrix 4×3 `{u}{v}{w}{o}`, mm, Z-up; Layer/GroupObject/ChildList rekursiv; Fixture: `GDTFSpec`, `GDTFMode`, `Addresses/Address break` (absolut oder `U.A`, Universe ab 1), `FixtureIDNumeric`, `DMXInvertPan/Tilt`; Szene-Objekte über `Geometry3D` (fileName + Matrix) und `Symbol` → `Symdef`
- **GDTF:** Meter; Matrix 4×4 zeilenweise, Translation in 4. Spalte; Gerät hängend modelliert, Pan um Z, Tilt um X; Beam strahlt −Z; Model-Maße skalieren das Mesh; Beam: `BeamAngle`, `FieldAngle`, `BeamRadius`, `BeamType`, `LuminousFlux`
- **DMX:** `Offset "1,2"` MSB zuerst; DMXValue `n/b` (Byte-Mirroring, `s` = Shift); ChannelFunction `DMXFrom`, `Default`, `PhysicalFrom/To`, `RealFade`, `RealAcceleration`, `Wheel`, `ModeMaster`; ChannelSet `WheelSlotIndex` (1-basiert); Relations `Multiply`
- **Räder:** Slot-Farbe CIE xyY, `MediaFileName`, Prisma-`Facet` (2D-Affin 3×3)
- **Gobo-PNG:** Transmission = Alpha × Helligkeit (transparent = Halter)

## Phasen

### Phase 0 – Grundlage und Library-Test
1. Rust ≥ 1.85, Abhängigkeiten anlegen *(erledigt: Pin entfernt, `clap` für die CLI, Art-Net-Crates; `dmx/` angelegt, übrige Module entstehen mit ihrer Phase)*; Module `app/`, `render/`, `io/{mvr, models}`, `dmx/{artnet, resolve}`, `sim/{motion, state}`, `demo.rs`
2. *(erledigt, Ergebnis in SPEC 13.1: `gdtf` mit Highlight-Ergänzung, eigener Resolver statt `zeevonk`)* Spike gegen `testdata/`: `gdtf` parst alle GDTFs, `ds3`/`gltf` laden alle Modelle, `zeevonk` an 1–2 Fixtures → Entscheidung `zeevonk` vs. eigener Resolver; Ergebnis in [SPEC.md](SPEC.md)

### Phase 1 – Art-Net *(parallel zu 2 und 3)* – umgesetzt
3. Empfangs-Thread: UDP 6454 via `socket2` (Reuse, Broadcast, Interface wählbar), Parsing mit `artnet_protocol`, Universe-Puffer + Statistik
4. ArtPoll → ArtPollReply (Universen aus Patch, je 4 Ports + BindIndex, Node-Name)
5. Universe-Offset einstellbar (Default: MVR-Universe 1 = Art-Net 0); `dmx::UniverseMap`, CLI/Panel folgen mit MVR-Patch
6. `examples/artnet_send.rs` als Test-Sender

### Phase 2 – GDTF-Auswertung
7. Fixture-Typ → Render-Typ: Geometrie-Kette (Pan/Tilt-Achse aus DMX-Kanal-Geometry), Beam-Parameter, Modelle pro Geometrie (auf Model-Maße skaliert), Räder (Gobo-Layer, Farben CIE → RGB, Prismen-Facetten)
8. Gobo-Import: Alpha × Helligkeit, Dedupe, ein Texture-Array (Layer-Limit prüfen)
9. Tests mit `testdata/`

### Phase 3 – MVR *(eigen)* – umgesetzt (SPEC 13.2)
10. `io/mvr.rs`: Layer/Gruppen rekursiv, Matrizen kumulieren; Fixtures (UUID, Name, FID, GDTF + Mode, Adressen/Breaks, Invert); Szene-Objekte → Meshes
11. Koordinaten Z-up → Y-up `(x, z, −y)`, mm → m
12. Tests: Matrix-Parsing, Adressformate, Beispiel-MVR

### Phase 4 – DMX → Attribute *(braucht 0, 2, 3)*
13. `dmx/resolve.rs`: Breaks, 8/16/24 Bit, aktive ChannelFunction inkl. ModeMaster, physikalischer Wert, ChannelSet → Slot, Defaults, Relations Multiply
14. Mapping auf Zielzustand: Pan/Tilt (Invert), Dimmer, Shutter/Strobe, Farbe (RGB/W, CMY, CTO), Farbrad (Split), Gobo n (Slot, Index-Winkel oder Rotationsgeschwindigkeit), Prisma (Index/Rotation), Zoom, Focus, Iris, Frost
15. Tests: 16 Bit, Mirroring, Funktionswahl, Wheel-Sets, ModeMaster

### Phase 5 – Simulation
16. `sim/motion.rs`: Ziel → Ist mit `v_max = Bereich / RealFade`, `a_max = v_max / RealAcceleration`; Defaults Pan 540° / 2,5 s, Tilt 270° / 1,8 s, Beschleunigung 0,3 s; Radfahrt, Rotationsintegration, Strobe, DMX-Glättung; Tests

### Phase 6 – Renderer *(braucht 2, 3, 5)*
17. Kinematik: Welt = MVR-Matrix × Geometrie-Positionen × Achsrotationen → Beam-Basis; Shadow-Origin = Tilt-Pivot in Grundstellung
18. Beam-Parameter aus GDTF (Wash = weiche Kante), Zoom überschreibt
19. Lichtbild RGBA: n Gobo-Räder, Farbrad (Split), Iris, Frost; Mischfarbe/Dimmer als Strahlfarbe
20. Prisma-Facetten aus GDTF als Strahl-Instanzen
21. Fixture-Meshes pro (Typ, Geometrie) instanziert, Linse emissiv
22. Statische MVR-Szene + optionaler Boden + Schatten

### Phase 7 – App und Bedienung *(braucht 1, 6)*
23. Kamera (eigen, ~100 Zeilen): Orbit (linke Maus), Verschieben (mittlere Maus / Shift), Zoom (Mausrad); beim MVR-Laden automatisch aufs Rig einpassen; Ansichten per Taste (FOH, oben, links, rechts); Eingaben über dem Panel gehen an egui
24. egui-Panel: Art-Net-Status (Universen, Pakete/s, Quellen), Patch-Liste (FID, Name, Typ, Mode, U.A), DMX-Monitor + dekodierte Attribute, Render-Einstellungen (Qualität, Haze, Belichtung, Boden, Schatten), Universe-Offset, Interface
25. MVR laden per `--mvr` und Drag & Drop, Neu-Laden; `--demo` bleibt

### Phase 8 – Verifikation
26. `cargo test` + `cargo clippy` ohne Warnungen
27. `artnet_send` gegen laufenden Visualizer
28. `testdata/`-MVR gegen grandMA3 3D / MagicQ Vis: Positionen, Ausrichtung; Pult-Art-Net: Pan/Tilt-Richtung und -Bereich, Gobo, Farbe, Prisma, Zoom
29. Pult auf demselben PC und im Netzwerk
30. Leistung mit komplettem Rig gegen [SPEC.md](SPEC.md)

## Nicht enthalten

sACN, MVR-xchange, LED-Multicell, Animationsrad, Shaper/Blades, Video/Media-Server, Laser, Rig-Editor, mehrere Beams pro Fixture.

## Hinweise

- Unicast an `127.0.0.1` erreicht nur ein Programm → im Pult Broadcast empfehlen; Panel zeigt Hinweis, wenn keine Pakete ankommen.
- Python-Version ab Phase 6 einfrieren (gemeinsame Shader werden erweitert).
- Testdaten: `testdata/` (MIT, im Repo) und `testdata/external/` (per `python tools/fetch_testdata.py`, nicht im Repo), Details in [testdata/README.md](testdata/README.md).
