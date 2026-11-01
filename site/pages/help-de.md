+++
title = "Hilfe"
+++

So benutzt du Loquax, die Messenger-App. [This page in English](/help).

## Die ersten Schritte

Beim ersten Start fragt Loquax, wo dein Postfach sein soll. Dein Postfach empfängt deine Nachrichten und bewahrt sie für dich auf.

- "Auf diesem Telefon" ist sofort startklar. Nachrichten erreichen dich, solange das Telefon an und online ist.
- "Auf einem Computer zu Hause" heißt: Nachrichten kommen auch an, wenn dein Telefon aus ist. Du brauchst dafür einen Computer, der eingeschaltet bleibt, siehe "Das Postfach" weiter unten.

Du kannst auf dem Telefon anfangen und dein Postfach später auf einen Computer umziehen, ohne etwas zu verlieren.

Danach zeigt Loquax deine Adresse, über die andere dir schreiben, und fragt nach einem Namen, den andere sehen. Sichere deine Adresse, wenn Loquax es anbietet: ohne Sicherung bedeutet ein verlorenes Telefon eine neue Adresse.

Erlaube Loquax, im Hintergrund zu laufen, wenn Android fragt. Sonst hält Android die App an, wenn das Telefon still liegt, und deine Nachrichten kommen zu spät.

## Einen Kontakt hinzufügen

Jeder in Loquax hat einen Kontaktcode, als quadratischen QR-Code im eigenen Profil. Tippe oben in der Chatliste auf dein eigenes Bild, dann siehst du deinen.

- **Am selben Ort:** Tippe in deinem Profil auf "Code eines Kontakts scannen" und halte die Kamera auf das Quadrat des anderen Telefons. Loquax fragt, wie du den Kontakt nennst, und öffnet dann den Chat.
- **Weit weg:** Bitte die andere Person, in ihrem Profil auf "Teilen" zu tippen und dir den Text zu schicken, per E-Mail oder über einen anderen Messenger. Er beginnt mit `lxma://`. Kopiere ihn, tippe in der Chatliste auf den Stift, füge ihn in das Feld ein und bestätige den Namen.
- **Jemanden einladen:** Tippe in deinem Profil auf "Teilen" und schicke den Text, oder auf "Als Bild teilen", um das Quadrat selbst zu schicken.

Codes aus Columba und Sideband funktionieren genauso.

## Fotos, Dateien und Sprachnachrichten senden

Noch nicht. Heute sendet Loquax Text, Emoji und deine Position. Fotos, Dateien und Sprachnachrichten kommen, und diese Seite sagt, wie es geht, sobald sie da sind.

## Deine Position senden

Tippe im Chat auf die Büroklammer, dann auf "Meine Position". Loquax fragt einmal, ob es deinen Standort verwenden darf, und schickt dann genau dieser einen Person, wo du gerade bist. Sonst sieht es niemand, und Loquax sendet deine Position nur, wenn du tippst.

Auch auf der Karte kannst du teilen: "Meine Position teilen" fragt, an wen sie gehen soll.

Die Position eine Weile lang teilen, so dass sie sich von selbst aktualisiert, kann die App noch nicht. Bis dahin teile sie einfach noch einmal, wenn du woanders bist. Wenn die App eines Freundes die eigene Position laufend schickt, markiert Loquax sie mit "live" und zeigt die Person auf der Karte.

## Das Postfach

Dein Postfach ist ein Computer zu Hause, der eingeschaltet bleibt, zum Beispiel ein Raspberry Pi, und deine Nachrichten aufbewahrt, während dein Telefon aus ist.

Installiere Loquax auf diesem Computer (wie, steht auf der [Postfach-Seite](/mailbox), auf Englisch) und starte den einen Befehl:

```
loquax mailbox setup
```

Drücke bei jeder Frage Enter für den Vorschlag. Am Ende zeigt er einen quadratischen Code. Wähle auf dem Telefon "Auf einem Computer zu Hause", dann "Code scannen", und halte die Kamera auf das Quadrat. Das Telefon verbindet sich von selbst mit deinem Postfach.

Du nutzt Loquax schon mit dem Postfach auf dem Telefon? Öffne dein Profil und tippe auf "Anderes Postfach verwenden".

## Ein zweites Gerät

Ein Tablet oder ein zweites Telefon kann dasselbe Postfach lesen, wenn das Postfach auf einem Computer läuft. Starte `loquax mailbox setup` auf dem Computer noch einmal: jeder Code gilt für ein Gerät, einmal. Installiere dann Loquax auf dem zweiten Gerät und scanne den neuen Code wie oben.

Beide Geräte sehen danach dieselben Chats. Wenn du eine Nachricht löschst, fragt Loquax, ob nur auf diesem Gerät oder auf allen Geräten des Postfachs.

## Was die Haken bedeuten

Deine eigenen Nachrichten tragen unten in der Ecke ein kleines Zeichen:

- **Eine Uhr:** Loquax sendet noch, oder hat gesendet und wartet auf Antwort.
- **Ein Haken:** Die Person, der du geschrieben hast, ist gerade nicht erreichbar. Die Nachricht wartet im Netz auf sie und erreicht sie, sobald ihr Telefon oder ihr Postfach sie abholt.
- **Zwei Haken:** zugestellt.
- **Zwei grüne Haken:** gelesen. Das siehst du nur, wenn die andere Person Lesebestätigungen erlaubt, und nur solange du sie auch erlaubst (Einstellungen, Privatsphäre).
- **Ein roter Kreis:** nicht gesendet. Tippe darauf, um es erneut zu senden.

## Wenn "Nicht verbunden" bleibt

Loquax sagt dir in seiner Benachrichtigung, ob es dein Postfach erreicht. Wenn dort dauerhaft "Nicht verbunden" steht (die App sagt es zur Zeit noch auf Englisch: "Not connected"):

1. Prüfe, ob das Telefon Internet hat, über WLAN oder mobile Daten.
2. Postfach auf einem Computer: Prüfe, ob der Computer an und online ist. Wenn die Einrichtung das Postfach mit dem Computer starten durfte, kommt es nach einem Stromausfall von selbst wieder.
3. Prüfe, ob Loquax im Hintergrund laufen darf: in Androids Einstellungen für Loquax, Akku, "Nicht eingeschränkt" wählen.
4. Immer noch nichts? Starte auf dem Computer `sudo systemctl restart loquax-mailbox` und warte eine Minute.
5. Wenn der Computer in ein anderes Netz umgezogen ist und du bei der Einrichtung eine feste Adresse gewählt hast, starte `loquax mailbox setup` noch einmal und scanne den neuen Code.

Was du in der Zwischenzeit schreibst, bleibt erhalten und geht raus, sobald die Verbindung wieder steht.

## Wie es funktioniert, falls du es wissen willst

Loquax braucht keine Server einer Firma. Jede Nachricht ist für genau die eine Person verschlüsselt, für die sie bestimmt ist, und reist durch ein Mesh aus Computern, Telefonen und kleinen Funkplatinen, die sie weiterreichen: über das Internet, WLAN, Bluetooth, oder über LoRa-Funk, wo es gar kein Internet gibt. Deine Adresse leitet sich aus einem Schlüssel ab, den nur dein Gerät kennt. Die Einzelheiten stehen in der [Entwicklerdokumentation](https://leviculum.network/docs/) (auf Englisch).
