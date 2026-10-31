# Testdaten

Für Spike und Tests (`cargo test`). Alle Dateien unverändert übernommen.

## Im Repo (MIT)

| Datei | Inhalt | Quelle |
|---|---|---|
| `basic_fixture.mvr` | 1 LED-PAR, nicht gepatcht | [open-stage/python-mvr](https://github.com/open-stage/python-mvr) `tests/`, Commit `9bbcabc`, [LICENSE](LICENSE-python-mvr) |
| `capture_demo_show.mvr` | Capture-Export: 76 Fixtures (Clay Paky Alpha Spot QWO 800, A.leda Wash K20, Robe MMX Spot/WashBeam, ADB ALC4), 6 Universen, Traversen und Bühne als 3DS | wie oben |
| `scene_objects.mvr` | 72 Fixtures, Szene-Objekte als GLB | wie oben |
| `BlenderDMX@LED_PAR_64_RGBW@v0.3.gdtf` | einfache RGBW-PAR mit Modellen | [open-stage/python-gdtf](https://github.com/open-stage/python-gdtf) `tests/`, Commit `5252bcc`, [LICENSE](LICENSE-python-gdtf) |

Die GDTFs in den Capture-Exporten enthalten keine Räder und Modelle.

## Per Download (`python tools/fetch_testdata.py` → `testdata/external/`, nicht im Repo)

| Datei | Inhalt | Quelle, Lizenz |
|---|---|---|
| `basic_gdtf.mvr` | Robe MegaPointe mit 7 Rädern, 61 Prisma-Facetten, 3DS-Modellen (Rad-Bilder fehlen) | [mvrdevelopment/spec](https://github.com/mvrdevelopment/spec) `proposal/mvr-update-for-gdtf12/`, ISC laut `package.json` |
| `BlenderDMX@BeskydBeam@version10.gdtf` | 2 Gobo-Räder mit 22 Bildern | [open-stage/blender-dmx](https://github.com/open-stage/blender-dmx) `assets/profiles/`, GPL-3.0 |

Das Skript prüft SHA-256. Tests laufen auch ohne diese Dateien.
