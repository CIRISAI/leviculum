+++
title = "Hilfe"
+++

So benutzt du Loquax, die Messenger-App. [This page in English](/help).

## Die ersten Schritte

Beim ersten Start fragt Loquax, wo dein Postfach sein soll. Dein Postfach empfängt deine Nachrichten und bewahrt sie für dich auf.

- "Auf diesem Handy" ist sofort startklar. Nachrichten erreichen dich, solange das Handy an und online ist.
- "Auf einem Computer zu Hause" heißt: Nachrichten kommen auch an, wenn dein Handy aus ist. Du brauchst dafür einen Computer, der eingeschaltet bleibt, siehe "Das Postfach" weiter unten.

Du kannst auf dem Handy anfangen und dein Postfach später auf einen Computer umziehen, ohne etwas zu verlieren.

Danach zeigt Loquax deine Adresse, über die andere dir schreiben, und fragt nach einem Namen, den andere sehen. Sichere deine Adresse, wenn Loquax es anbietet: ohne Sicherung bedeutet ein verlorenes Handy eine neue Adresse.

Erlaube Loquax, im Hintergrund zu laufen, wenn Android fragt. Sonst hält Android die App an, wenn das Handy still liegt, und deine Nachrichten kommen zu spät.

## Einen Kontakt hinzufügen

Jeder in Loquax hat einen Kontaktcode, als quadratischen QR-Code im eigenen Profil. Tippe oben in der Chatliste auf dein eigenes Bild, dann siehst du deinen.

- **Am selben Ort:** Tippe in deinem Profil auf "Code eines Kontakts scannen" und halte die Kamera auf das Quadrat des anderen Handys. Loquax fragt, wie du den Kontakt nennst, und öffnet dann den Chat.
- **Weit weg:** Bitte die andere Person, in ihrem Profil auf "Teilen" zu tippen und dir den Text zu schicken, per E-Mail oder über einen anderen Messenger. Er beginnt mit `lxma://`. Kopiere ihn, tippe in der Chatliste auf den Stift, füge ihn in das Feld ein und bestätige den Namen.
- **Jemanden einladen:** Tippe in deinem Profil auf "Teilen" und schicke den Text, oder auf "Als Bild teilen", um das Quadrat selbst zu schicken.

Codes aus Columba und Sideband funktionieren genauso.

## Fotos, Dateien und Sprachnachrichten senden

- **Ein Foto:** Tippe im Nachrichtenfeld auf die Kamera, um eines aufzunehmen, oder auf das Plus daneben, dann auf "Galerie". Vor dem Senden kannst du eine Bildunterschrift hinzufügen. Loquax verkleinert Fotos vor dem Senden, Ort und Kameradaten gehen nie mit. Wie klein, steht unter Einstellungen, Chats, "Fotogröße".
- **Eine Datei:** das Plus, dann "Datei".
- **Eine Sprachnachricht:** Halte das Mikrofon, während du sprichst, und lass los, um zu senden. Nach links wischen bricht ab, nach oben wischen stellt die Aufnahme fest, dann kannst du den Knopf loslassen.

## Deine Position senden

Tippe im Chat auf das Positionszeichen im Nachrichtenfeld. Loquax fragt einmal, ob es deinen Standort verwenden darf, und bietet dann "Jetzt, einmal" an, oder für 15 Minuten, 1 Stunde, 8 Stunden oder "Bis ich es beende". Nur diese eine Person sieht sie, und Loquax sendet deine Position nur, wenn du es so wählst.

Solange du teilst, zeigt der Chat, wie lange noch, und einen Knopf "Beenden". Unter Einstellungen, Privatsphäre, "Positionsfreigabe" steht jeder, mit dem du gerade teilst.

Auch auf der Karte kannst du teilen: "Meine Position teilen" fragt, an wen sie gehen soll. Teilt ein Freund die eigene Position, markiert Loquax ihn mit "live" und zeigt ihn auf der Karte.

## Gruppen

Tippe in der Chatliste auf den Stift, dann auf "Neue Gruppe". Wähle die Leute aus, tippe auf "Weiter", gib der Gruppe einen Namen und tippe auf "Erstellen". Alle, die du ausgewählt hast, bekommen eine Einladung mit dem Knopf "Beitreten".

Eine Gruppe liegt im Postfach der Person, die sie erstellt hat, und dieses Postfach kann die Nachrichten der Gruppe lesen. Loquax sagt das auf der Einladung, damit du weißt, wem du vertraust.

Im Chat der Gruppe zeigt das Menü unter "Mitglieder", wer dabei ist. Dort kannst du die Gruppe verlassen; wer sie erstellt hat, kann außerdem Mitglieder hinzufügen und entfernen und die Gruppe für alle löschen.

Freunde mit einer anderen App, zum Beispiel Columba oder Sideband, können auch in einer Gruppe sein: Sie bekommen die Einladung und die Nachrichten der Gruppe als gewöhnliche Nachrichten.

## Das Postfach

Dein Postfach ist ein Computer zu Hause, der eingeschaltet bleibt, zum Beispiel ein Raspberry Pi, und deine Nachrichten aufbewahrt, während dein Handy aus ist.

Installiere Loquax auf diesem Computer (wie, steht auf der [Postfach-Seite](/mailbox), auf Englisch) und starte den einen Befehl:

```
loquax mailbox setup
```

Drücke bei jeder Frage Enter für den Vorschlag. Am Ende zeigt er einen quadratischen Code. Wähle auf dem Handy "Auf einem Computer zu Hause", dann "Code scannen", und halte die Kamera auf das Quadrat. Das Handy verbindet sich von selbst mit deinem Postfach.

Du nutzt Loquax schon mit dem Postfach auf dem Handy? Öffne dein Profil und tippe auf "Anderes Postfach verwenden".

## Ein zweites Gerät

Ein Tablet oder ein zweites Handy kann dasselbe Postfach lesen.

- **Postfach auf deinem Handy:** Tippe auf dein Bild, dann auf "Gerät verbinden". Installiere Loquax auf dem anderen Gerät, wähle "Einem bestehenden Postfach beitreten" und scanne den Code.
- **Postfach auf einem Computer:** Starte `loquax mailbox setup` auf dem Computer noch einmal: jeder Code gilt für ein Gerät, einmal. Installiere dann Loquax auf dem zweiten Gerät und scanne den neuen Code wie oben.

Ein neues Handy, das das alte ersetzt, ist ein Umzug, kein zweites Gerät: Tippe auf dem alten Handy auf dein Bild, dann auf "Auf ein neues Handy umziehen". Deine Adresse, deine Kontakte und deine Nachrichten ziehen mit um.

Beide Geräte sehen danach dieselben Chats. Wenn du eine Nachricht löschst, fragt Loquax, ob nur auf diesem Gerät oder auf allen Geräten des Postfachs.

## Was die Haken bedeuten

Deine eigenen Nachrichten tragen unten in der Ecke ein kleines Zeichen:

- **Eine Uhr:** Loquax sendet noch, oder hat gesendet und wartet auf Antwort.
- **Ein Haken:** Die Person, der du geschrieben hast, ist gerade nicht erreichbar. Die Nachricht wartet im Netz auf sie und erreicht sie, sobald ihr Handy oder ihr Postfach sie abholt.
- **Zwei Haken:** zugestellt.
- **Zwei grüne Haken:** gelesen. Das siehst du nur, wenn die andere Person Lesebestätigungen erlaubt, und nur solange du sie auch erlaubst (Einstellungen, Privatsphäre).
- **Ein roter Kreis:** nicht gesendet. Tippe darauf, um es erneut zu senden.

## Wenn "Nicht verbunden" bleibt

Loquax sagt dir in seiner Benachrichtigung, ob es dein Postfach erreicht. Wenn dort dauerhaft "Nicht verbunden" steht (die App sagt es zur Zeit noch auf Englisch: "Not connected"):

1. Prüfe, ob das Handy Internet hat, über WLAN oder mobile Daten.
2. Postfach auf einem Computer: Prüfe, ob der Computer an und online ist. Wenn die Einrichtung das Postfach mit dem Computer starten durfte, kommt es nach einem Stromausfall von selbst wieder.
3. Prüfe, ob Loquax im Hintergrund laufen darf: in Androids Einstellungen für Loquax, Akku, "Nicht eingeschränkt" wählen.
4. Immer noch nichts? Starte auf dem Computer `sudo systemctl restart loquax-mailbox` und warte eine Minute.
5. Wenn der Computer in ein anderes Netz umgezogen ist und du bei der Einrichtung eine feste Adresse gewählt hast, starte `loquax mailbox setup` noch einmal und scanne den neuen Code.

Was du in der Zwischenzeit schreibst, bleibt erhalten und geht raus, sobald die Verbindung wieder steht.

## Wie es funktioniert, falls du es wissen willst

Loquax braucht keine Server einer Firma. Jede Nachricht ist für genau die eine Person verschlüsselt, für die sie bestimmt ist, und reist durch ein Mesh aus Computern, Telefonen und kleinen Funkplatinen, die sie weiterreichen: über das Internet, WLAN, Bluetooth, oder über LoRa-Funk, wo es gar kein Internet gibt. Deine Adresse leitet sich aus einem Schlüssel ab, den nur dein Gerät kennt. Die Einzelheiten stehen in der [Entwicklerdokumentation](https://leviculum.network/docs/) (auf Englisch).
