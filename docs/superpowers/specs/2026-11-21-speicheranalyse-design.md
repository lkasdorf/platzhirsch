# Design: Schneller Speicherplatz-Analysator für Windows

Stand: 2026-11-21. Arbeitsname: **Platzhirsch** (Vorschlag, jederzeit änderbar).

Kennzeichnung in diesem Dokument: **[Leon]** = so entschieden. **[Vorschlag]** = von Claude
festgelegt, nicht abgestimmt, darf korrigiert werden.

## 1. Absicht

Ein eigenes Werkzeug, das zeigt, welche Ordner auf einem Windows-Laufwerk wie viel Platz
belegen: Ordnerbaum mit Größen plus Treemap.

Warum: TreeSize Free braucht auf dem großen Laufwerk lange (C: mit 2,5 TB belegt,
1,68 Mio. Dateien, 239.000 Ordner) und verweist für mehr Tempo auf die kostenpflichtige
Version (ab 41 € pro Nutzer und Jahr).

Schwerpunkt **[Leon]**: vor allem ein schneller Scanner, dazu eine schlanke Oberfläche.

## 2. Ziele, Nicht-Ziele, Anti-Ziele

### Ziele

1. **Tempo.** Ein ganzes NTFS-Laufwerk wird über die Master File Table (MFT) gelesen.
   Zielwert **[Vorschlag]**: C: (NVMe-SSD, ca. 2 Mio. Einträge) vom Klick bis zum fertigen
   Baum in höchstens 10 Sekunden.
2. **Schlanke Oberfläche.** Ein Fenster: Leiste oben, Ordnerbaum als Tabelle, Treemap
   darunter, Statuszeile. Keine Ribbons, keine Dialogketten.
3. **Eine einzelne .exe**, kein Installer, keine Laufzeit-Abhängigkeit. Zielwert
   **[Vorschlag]**: höchstens 15 MB.
4. **Funktioniert auch ohne Admin-Rechte**, dann langsamer (Rückfallweg).
5. **Flüssige Bedienung** auch bei 2 Mio. Einträgen: Scrollen und Aufklappen ohne Ruckeln.
   Arbeitsspeicher **[Vorschlag]**: höchstens 400 MB bei 2 Mio. Einträgen, gemessen nach
   Ende des Scans. Während des Scans darf die Spitze bis 1 GB gehen.
6. **Zahlen, denen man trauen kann.** Im Schnellweg weicht die Summe „Belegt" eines ganzen
   Laufwerks höchstens 2 % von (Gesamtgröße − freier Platz) ab, die Windows meldet. Der
   Rückfallweg liefert „Belegt" als Näherung und sagt das auch (siehe 5.3).

### Nicht-Ziele für Version 1 **[Vorschlag]**

- Duplikatsuche, Dateisuche, Filter nach Typ oder Alter
- Exporte (PDF, Excel, HTML), Berichte, Kommandozeile, geplante Scans
- Löschen, Verschieben, Umbenennen aus dem Programm heraus
- Schnellweg für andere Dateisysteme als NTFS (ReFS, exFAT, FAT32 nutzen den Rückfallweg)
- Baum, der sich während eines laufenden Scans live füllt
- Aktualisieren ohne Neu-Scan (USN-Journal)
- Mehrere Laufwerke gleichzeitig in einem Baum
- Mehrsprachigkeit, Themes, gespeicherte Einstellungen
- Zweite und weitere Hardlinks einer Datei einzeln anzeigen

### Anti-Ziele

- **Niemals auf das gescannte Laufwerk schreiben.** Laufwerk und Dateien werden nur lesend
  geöffnet.
- **Kein Netzwerkzugriff**, keine Telemetrie, keine Update-Prüfung, keine Werbe-Dialoge.
- **Kein Nachbau von Name, Logo oder Optik von TreeSize.** Eigener Name, eigenes Icon,
  eigene Farben. Nichts aus TreeSize wird dekompiliert oder übernommen.
- **Keine stillen Falschzahlen.** Was nicht gelesen werden konnte, wird sichtbar markiert und
  nicht als 0 Bytes ausgegeben.

## 3. Rahmenbedingungen

Am Rechner geprüft (2026-11-21):

- Windows 11 Pro, x64. Rust 1.99.1, Toolchain `stable-x86_64-pc-windows-msvc`.
  Visual-Studio-Verzeichnis unter `C:\Program Files (x86)` vorhanden; ob der MSVC-Linker
  funktioniert, zeigt erst der erste Build.
- C: NTFS, NVMe-SSD, 3,8 TB. F: NTFS, SSD über USB, 3,7 TB.
- .NET SDK ist nicht installiert (spielt für die gewählte Lösung keine Rolle).
- Die Entwicklungs-Shell läuft **nicht** als Administrator. Der Schnellweg lässt sich also
  nur testen, wenn Leon die UAC-Abfrage bestätigt.
- Projektordner `C:\Users\LeonKasdorf\dev\TreeSize` ist leer und kein Git-Repository.

Technische Grenzen:

- Der MFT-Schnellweg braucht Admin-Rechte (Lesezugriff auf `\\.\C:`) und NTFS.
- Zielplattform ist Windows 10/11 x64. Andere Plattformen sind nicht vorgesehen.

Für wen **[Annahme, bitte bestätigen]**: ein Werkzeug für Leons eigene Rechner. Daraus
folgt: keine Code-Signatur, kein Installer, abgenommen wird auf C: und F:. Der MFT-Leser
nimmt trotzdem alle Größen (Sektor, Cluster, Record) aus dem Bootsektor und setzt keine
festen Werte voraus, damit er auch auf anderen Datenträgern stimmt.

## 4. Lösungswege

### Womit gebaut wird

| | Rust + native Oberfläche (egui) | Rust-Kern + Web-Oberfläche (Tauri) | C# / .NET |
|---|---|---|---|
| Scan-Tempo | am schnellsten | gleich schnell | etwas langsamer, mehr Speicher |
| Auslieferung | eine .exe, ca. 5–10 MB | ca. 10 MB, braucht WebView2 | 70 MB+ oder .NET-Laufzeit |
| Oberfläche | Baum und Treemap selbst gezeichnet | am leichtesten hübsch zu machen | klassische Windows-Optik |
| Haken | Feinschliff der Optik kostet Mühe | 2 Mio. Einträge müssen häppchenweise an die Oberfläche | SDK nicht installiert |

**Gewählt [Leon]: Rust mit egui.** Passt am besten zu „schnell und schlank": eine Datei,
kein Umweg zwischen Scanner und Anzeige, Rust ist installiert. Die Treemap muss ohnehin
selbst gezeichnet werden, eine Web-Oberfläche brächte dafür wenig.

### Wie gescannt wird

| Weg | Tempo | Braucht | Liefert |
|---|---|---|---|
| A. MFT roh lesen und selbst auswerten | Sekunden | Admin, NTFS | Namen, Eltern, Größen, Zeitstempel |
| B. `FSCTL_ENUM_USN_DATA` | Sekunden | Admin, NTFS | Namen und Eltern, **keine Größen** |
| C. Ordner parallel durchlaufen | Minuten bei 2 Mio. Dateien | nichts | alles, überall |

B scheidet aus, weil die Größen fehlen und jede Datei einzeln nachgefragt werden müsste.

**Gewählt [Vorschlag, am 21.11. im Gespräch genannt, nicht ausdrücklich bestätigt]:
A als Schnellweg, C als Rückfallweg, beide im selben Programm.**

## 5. Aufbau

Ein Rust-Paket mit Bibliothek und Programm, damit Scanner und Datenmodell ohne Oberfläche
testbar sind **[Vorschlag]**:

```
src/
  main.rs        Start, Argumente, Fenster
  lib.rs
  model.rs       Baum im Speicher, Summen, Sortierung
  scan/
    mod.rs       Auswahl des Wegs, Fortschritt, Abbruch
    mft.rs       Schnellweg: Laufwerk öffnen, MFT lesen, Records auswerten
    walk.rs      Rückfallweg: Ordner parallel durchlaufen
  treemap.rs     Flächenaufteilung (reine Rechnung, ohne Oberfläche)
  ui/            Fenster, Tabelle, Treemap-Anzeige, Leisten
  win.rs         Windows-Aufrufe: Laufwerksliste, Admin-Status, Neustart als Admin
```

Der Scan läuft in Hintergrund-Threads. Die Oberfläche bekommt Fortschritt über atomare
Zähler und das fertige Ergebnis als unveränderlichen Baum (`Arc`). Ein laufender Scan lässt
sich abbrechen.

### 5.1 Datenmodell (`model.rs`)

Beide Scanner liefern dasselbe Ergebnis. Um 2 Mio. Einträge klein zu halten:

- Flache Arrays statt verketteter Knoten. Pro Eintrag: Eltern-Index, Name (Verweis in einen
  gemeinsamen Zeichenpuffer, UTF-8), Größe, belegter Platz, Änderungszeit, Merker
  (Ordner, nicht lesbar, Verknüpfung).
- Kinderlisten als zusammenhängender Bereich pro Ordner (Start und Anzahl).
- Summen pro Ordner, einmal nach dem Scan von unten nach oben berechnet, ohne Rekursion:
  Größe, belegt, Anzahl Dateien, Anzahl Ordner, neueste Änderung im Teilbaum.
- Sortierung der Kinder erst beim Anzeigen eines Ordners, danach gemerkt.
- Zielwert: im Schnitt höchstens ca. 150 Bytes pro Eintrag.

Begriffe:

- **Größe** = logische Länge des Dateiinhalts.
- **Belegt** = tatsächlich auf dem Datenträger reservierter Platz.

### 5.2 Schnellweg (`scan/mft.rs`)

Ablauf:

1. `\\.\X:` lesend öffnen (mit Freigabe für Lesen und Schreiben anderer). Bootsektor lesen:
   Sektorgröße, Clustergröße, Position der MFT, Record-Größe.
2. Die Lage der MFT selbst bestimmen. Sie kann in vielen Stücken auf dem Datenträger liegen.
   Record 0 (`$MFT`) allein reicht dafür nicht immer: bei stark zerstückelter MFT läuft die
   Liste der Datenläufe in Erweiterungs-Records über. Der Leser muss deshalb für `$MFT`
   entweder dessen `$ATTRIBUTE_LIST` auswerten oder die Stücke über
   `FSCTL_GET_RETRIEVAL_POINTERS` abfragen (Wahl beim Umsetzenden, am echten Laufwerk
   prüfen). **Pflichtprüfung:** Decken die gefundenen Stücke weniger ab als die in Record 0
   angegebene Größe der MFT, bricht der Schnellweg mit Fehlermeldung ab. Eine abgeschnittene
   MFT würde sonst stillschweigend Dateien unterschlagen.
3. MFT in großen, sektorgenauen Blöcken lesen. Auswertung parallel zum Lesen.
4. Pro Record: Fixups anwenden, Signatur und „in Benutzung" prüfen, Attribute auswerten.
5. Baum aufbauen, Summen berechnen.

Auszuwertende Attribute:

- `$STANDARD_INFORMATION`: Änderungszeit, Dateiattribute.
- `$FILE_NAME`: Eltern-Record und Name. Reine DOS-Kurznamen (8.3) werden ignoriert.
- `$DATA`: Größe und belegter Platz. Bei im Record liegenden Daten zählt die Länge als
  Größe und 0 als belegt. Bei komprimierten oder Sparse-Dateien zählt das Feld für den
  tatsächlich belegten Platz.

Fälle, die für richtige Zahlen zählen:

- **Erweiterungs-Records.** Stark fragmentierte Dateien verteilen ihre Attribute auf mehrere
  Records. Lösung: alle Records durchgehen und Attribute eines Erweiterungs-Records dem
  Basis-Record gutschreiben. Von mehrteiligen `$DATA`-Attributen zählt nur das Stück mit
  Start-VCN 0, weil dort die Gesamtgrößen stehen. `$ATTRIBUTE_LIST` muss dafür nicht
  gelesen werden.
- **Zusätzliche Datenströme.** Größe = nur der unbenannte Strom. Belegt = Summe über alle
  `$DATA`-Ströme. Grund: von Windows komprimierte Systemdateien (CompactOS) halten ihre
  Daten in einem benannten Strom; ohne diese Regel wäre der Windows-Ordner zu klein.
  **Ausnahme:** `$BadClus` (Record 8) hat einen benannten Strom `$Bad`, der so groß ist wie
  das ganze Laufwerk. Er wird nicht gezählt, sonst kämen ca. 3,8 TB zum Wurzelordner dazu.
  Am echten Laufwerk gegenprüfen, ob weitere NTFS-Systemdateien so auffallen.
- **Hardlinks.** Eine Datei mit mehreren Namen wird einmal gezählt und unter dem Namen
  angezeigt, der in ihrem Record zuerst steht. Die Regel ist fest, aber willkürlich: bei
  Dateien, die sowohl in `System32` als auch in `WinSxS` hängen, können die beiden
  Ordnergrößen von TreeSize abweichen. Die Summe des Laufwerks stimmt trotzdem und
  übersteigt nie den belegten Platz.
- **Junctions und symbolische Links** stehen als eigene Einträge ohne Inhalt in der MFT.
  Sie werden angezeigt und markiert, ihr Ziel wird nicht mitgezählt.
- **OneDrive-Platzhalter** zeigen ihre volle Größe, aber 0 belegt. Das ist korrekt.
- **Systemdateien von NTFS** (`$MFT`, `$LogFile` usw.) werden im Wurzelordner angezeigt,
  weil sie echten Platz belegen.
- **Verwaiste oder kaputte Records** (Eltern fehlt, Kreis, Fixup passt nicht, Record ändert
  sich gerade) werden übersprungen und gezählt. Die Anzahl steht in der Statuszeile.
- Der Eigenbedarf von Ordnern (Index-Daten) wird nicht gezählt. Fällt unter die 2 %.

Das Laufwerk wird im laufenden Betrieb gelesen. Kleine Abweichungen durch gleichzeitige
Änderungen sind hinzunehmen.

### 5.3 Rückfallweg (`scan/walk.rs`)

Wird benutzt, wenn keine Admin-Rechte da sind, das Laufwerk kein NTFS ist, ein Netzpfad
gewählt wurde oder ein einzelner Ordner gescannt wird.

- Ordnerinhalte über `GetFileInformationByHandleEx` mit `FileIdBothDirectoryInfo` lesen.
  Das liefert pro Eintrag Größe und belegten Platz des Hauptinhalts in einem Aufruf.
- **„Belegt" ist hier eine Näherung [Vorschlag].** Die Werte stammen aus dem
  Ordnerverzeichnis. Sie enthalten keine benannten Ströme (von Windows komprimierte
  Systemdateien erscheinen daher zu klein) und können bei Hardlinks veraltet sein. Einzelne
  Nachfragen pro Datei würden den Rückfallweg deutlich bremsen und unterbleiben. Die
  Statuszeile zeigt in diesem Fall „Ordner-Scan, Belegt genähert". „Größe" und die
  Dateizahl sind exakt.
- Mehrere Threads mit gemeinsamer Arbeitsliste (ein Auftrag pro Ordner).
- Pfade mit `\\?\`-Präfix, damit lange Pfade funktionieren.
- Junctions und symbolische Links werden nicht verfolgt. Andere Reparse-Punkte
  (OneDrive-Ordner) werden normal durchlaufen, sonst fehlte der OneDrive-Inhalt.
- **Cloud-Ordner, die lokal noch nicht vollständig vorliegen**, werden nicht betreten und
  als „nur online" markiert. Das Auflisten könnte sonst den Cloud-Anbieter veranlassen,
  Inhalte nachzuladen und Platzhalter zu schreiben, was gegen „nie schreiben" und „kein
  Netzwerk" verstieße. Erkennung voraussichtlich über das Attribut
  `FILE_ATTRIBUTE_RECALL_ON_OPEN`; der Umsetzende prüft das gegen die
  Microsoft-Dokumentation. Dateiinhalte werden ohnehin nie geöffnet.
- Ordner ohne Zugriff werden als „nicht lesbar" markiert und in der Statuszeile gezählt.
- Bekannte Abweichung: Hardlinks werden hier mehrfach gezählt.

### 5.4 Admin-Rechte (`win.rs`) **[Vorschlag]**

Das Programm startet ohne UAC-Abfrage. Wählt man ein ganzes NTFS-Laufwerk und ist nicht
Administrator, erscheint in der Leiste ein Hinweis mit zwei Knöpfen:

- **„Als Administrator neu starten"**: startet das Programm neu mit UAC-Abfrage und
  übergibt das gewählte Laufwerk, sodass der Scan danach sofort losläuft.
- **„Langsam scannen"**: nimmt den Rückfallweg.

Grund: Wer nur einen Ordner prüfen will, soll nicht bei jedem Start eine UAC-Abfrage
bekommen.

Regel für die Wahl des Wegs:

| Auswahl | Admin | Dateisystem | Weg |
|---|---|---|---|
| Ganzes Laufwerk | ja | NTFS | Schnellweg |
| Ganzes Laufwerk | nein | NTFS | Hinweis mit beiden Knöpfen |
| Ganzes Laufwerk | egal | nicht NTFS | Rückfallweg |
| Einzelner Ordner, Netzpfad | egal | egal | Rückfallweg |

Scheitert der Schnellweg unterwegs, wird der Fehler angezeigt und der Rückfallweg angeboten.
Kein stiller Wechsel.

### 5.5 Oberfläche (`ui/`)

```
+--------------------------------------------------------------------------+
| [C: 3,8 TB v] [Ordner...] [Scannen]   Anzeige: (o) Belegt ( ) Größe      |
+--------------------------------------------------------------------------+
| Name                        | Belegt | Größe  | Dateien | Ordner | %     | Geändert  |
| v C:\                       | 2,5 TB | 2,7 TB | 1,68 M  | 239 T  | 100 % | 21.11.26  |
|   v DRIVE  ████████████     | 1,8 TB | 1,8 TB | 216.840 | 7.181  |  72 % | 21.11.26  |
|     > Photos ███████        | 1,0 TB | 1,0 TB | 141.418 | 1.779  |  57 % | 15.11.26  |
|     > JD     █████          | 764 GB | 764 GB |     249 |     34 |  42 % | 21.11.26  |
|   > Users  ██               | 378 GB | 596 GB | 985.083 | 131 T  |  15 % | 21.11.26  |
|   ...                                                                    |
+--------------------------------------------------------------------------+
| Treemap des gewählten Ordners                                            |
| +------------------------------+-----------------+---------+             |
| | Photos                       | JD              | Users   |             |
| | +-----------+------+         | +-------------+ | +--+--+ |             |
| | | Mobile... | ...  |         | | extracted   | | |  |  | |             |
| +------------------------------+-----------------+---------+             |
+--------------------------------------------------------------------------+
| Frei: 1,28 TB von 3,72 TB | 1.683.667 Dateien | MFT-Scan, 4,2 s | 3 übersprungen |
+--------------------------------------------------------------------------+
```

**Leiste oben**

- Laufwerksliste (feste und Wechsel-Laufwerke mit Größe), Knopf für Ordnerauswahl,
  Scannen/Abbrechen.
- Umschalter „Belegt / Größe". Er bestimmt, welche Zahl Balken, Prozent, Standard-Sortierung
  und Treemap steuert. Standard: Belegt.
- Während des Scans: Fortschrittsbalken (Schnellweg: gelesener Anteil der MFT;
  Rückfallweg: Zähler gefundener Dateien).

**Tabelle**

- Spalten: Name, Belegt, Größe, Dateien, Ordner, % vom Elternordner, Geändert.
- Standard-Sortierung absteigend nach der gewählten Zahl. Klick auf Spaltenkopf sortiert um.
- Balken hinter dem Namen, proportional zum Anteil am Elternordner.
- Dateien eines Ordners stehen einzeln unter den Unterordnern.
- Nur sichtbare Zeilen werden gezeichnet. Die Liste der sichtbaren Zeilen wird beim Auf-
  und Zuklappen neu gebildet, nicht in jedem Bild.
- Tastatur: Pfeiltasten bewegen und klappen auf/zu.
- Rechtsklick: „Im Explorer anzeigen", „Pfad kopieren". Sonst nichts.
- „Nicht lesbar" und Verknüpfungen sind am Eintrag erkennbar.

**Treemap**

- Zeigt den in der Tabelle gewählten Ordner, verschachtelt nach Ebenen
  (Squarified-Verfahren).
- Zu kleine Flächen (unter wenigen Pixeln) werden nicht weiter unterteilt.
- Farbe nach Ebene, eigene Palette.
- Mauszeiger darüber: Pfad und Größe. Klick: wählt den Eintrag in der Tabelle.
- Die Aufteilung wird nur bei neuer Auswahl, Umschalten oder Größenänderung berechnet.
- Trennlinie zur Tabelle verschiebbar, Treemap ausblendbar.

**Statuszeile**: freier und gesamter Platz, Anzahl Dateien, benutzter Weg und Dauer, Anzahl
übersprungener oder nicht lesbarer Einträge.

**Sprache der Oberfläche [Vorschlag]:** Deutsch, fest. Zahlen und Datum im deutschen Format.

## 6. Prüfung

Automatische Tests (laufen ohne Admin):

- MFT-Auswertung gegen von Hand gebaute Record-Bytes: Fixups, Daten im Record und außerhalb,
  Datenläufe (auch Lücken und negative Sprünge), Erweiterungs-Records, mehrere Namen,
  benannte Ströme, `$BadClus`, kaputte Records, Record-Größe 1 KB und 4 KB, MFT-Lage über
  mehrere Records verteilt, abgeschnittene MFT führt zum Abbruch.
- Datenmodell: Summen, Zählungen, Sortierung, verwaiste Einträge.
- Treemap: Flächen proportional, alles innerhalb des Rahmens, keine Überlappung.
- Rückfallweg: gegen einen temporär angelegten Ordnerbaum mit bekannten Größen.

Prüfung am echten Laufwerk (braucht Leon für die UAC-Abfrage):

- Schnellweg auf C: läuft durch, Dauer wird gemessen und mit dem 10-Sekunden-Ziel
  verglichen.
- Summe „Belegt" gegen (Gesamt − Frei) von Windows, Abweichung höchstens 2 %.
- Ein Ordner ohne Hardlinks (z. B. `C:\DRIVE\Photos`): Schnellweg und Rückfallweg liefern
  dieselbe Dateizahl und dieselbe „Größe". „Belegt" wird nebeneinander ausgegeben; die
  Abweichung wird berichtet, ist aber kein Abnahmekriterium (sehr kleine Dateien, die ganz
  im MFT-Record liegen, zählen im Schnellweg mit 0 und im Rückfallweg eventuell mit wenigen
  Bytes).
- Schnellweg auch auf F: (SSD über USB).
- Bedienprobe: alles aufklappen, scrollen, Treemap anklicken.

Wird ein Zielwert verfehlt, wird das gemeldet und nicht stillschweigend hingenommen.

## 7. Dem Umsetzenden überlassen

- Genaue Crates und Versionen (egui/eframe, Windows-Bindings, Thread-Pool, Ordner-Dialog).
  Aktuelle Dokumentation prüfen, nicht aus dem Gedächtnis arbeiten.
- Blockgrößen, Thread-Anzahl, interne Aufteilung der Module.
- Genaue Farben, Abstände, Icon, Wortlaut von Meldungen.
- Ob für die Tests zusätzlich ein kleines NTFS-Abbild (VHD) benutzt wird.
- Git-Repository anlegen und erste Commits (im Umsetzungsplan klären).

## 8. Von Claude getroffene Festlegungen im Überblick

Zum schnellen Gegenprüfen, alle änderbar:

1. Arbeitsname „Platzhirsch".
2. Oberfläche auf Deutsch, feste Sprache.
3. Start ohne UAC, Neustart als Administrator per Knopf.
4. Einzelne Ordner laufen immer über den Rückfallweg.
5. Kein Löschen aus dem Programm, nur „Im Explorer anzeigen" und „Pfad kopieren".
6. Ergebnis erscheint erst nach Ende des Scans, kein live wachsender Baum.
7. Hardlinks werden im Schnellweg einmal gezählt, weitere Namen nicht angezeigt.
8. Standard-Anzeige ist „Belegt", nicht „Größe".
9. Zielwerte: 10 s für C:, 15 MB Programmgröße, 400 MB Arbeitsspeicher nach dem Scan
   (Spitze 1 GB), 2 % Abweichung im Schnellweg.
10. Alles unter „Nicht-Ziele" bleibt in Version 1 draußen.
11. Nur für Leons eigene Rechner: keine Code-Signatur, kein Installer.
12. Im Rückfallweg ist „Belegt" eine gekennzeichnete Näherung.
13. Cloud-Ordner, die lokal nicht vollständig vorliegen, werden nicht betreten.
14. Bei Hardlinks gewinnt der Name, der im Record zuerst steht.

## 9. Gegenlesen

Ein unabhängiger Reviewer hat das Dokument am 21.11.2026 gegengelesen. Eingearbeitet:
Lage der MFT bei starker Zerstückelung (5.2 Schritt 2), Ausnahme `$BadClus`, Genauigkeit
des Rückfallwegs, Cloud-Ordner, Speicher-Zielwert, Hardlink-Regel. Die NTFS-Aussagen
beruhen auf Fachwissen und sind noch nicht an einem echten Laufwerk gemessen; das passiert
in der Prüfung nach Abschnitt 6.
