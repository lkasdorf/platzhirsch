# Changelog

Alle nennenswerten Änderungen an diesem Projekt werden in dieser Datei festgehalten.

Das Format basiert auf [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
und das Projekt folgt [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Schnellscan für NTFS-Laufwerke: liest die Master File Table direkt und braucht dafür
  Administratorrechte. Gemessen am 21.11.2026: C: mit 1,87 Mio. Einträgen in 2,1 s,
  Abweichung vom belegten Platz des Laufwerks 0,36 %.
- Rückfallweg ohne Administratorrechte: paralleler Ordner-Scan für einzelne Ordner,
  Netzpfade und Laufwerke ohne NTFS. „Belegt" ist dabei eine Näherung.
- Ordnerbaum im Speicher mit Summen für Größe, belegten Platz, Datei- und Ordnerzahl
  und neuester Änderung je Ordner.
- Kennzeichnung statt stiller Falschzahlen: nicht lesbare Ordner, Verknüpfungen
  (Junctions, symbolische Links) und nur online vorhandene Cloud-Ordner werden markiert
  und gezählt. Verknüpfungen werden nie verfolgt.
- Treemap-Aufteilung nach dem Squarified-Verfahren als Grundlage der Flächenansicht.
- Deutsche Formatierung für Größen, Zahlen, Prozent und Datum.
- Diagnose-Programm `bench` zum Messen von Scan-Dauer, Speicherbedarf und Abweichung
  vom belegten Platz des Laufwerks. Es wird nicht ausgeliefert.
- Wächter-Tests, die Schreibzugriffe auf gescannte Laufwerke und Netzwerk-Bibliotheken
  im Code verbieten.
