# Finding a cheat, by search

*How a Claude Code instance finds a game's lives counter — or its coins, its
energy, its money — with no code list, no database, and no prior knowledge of
the game. Written for the agent that will do it.*

There are no Game Genie codes here. The player says what they want in plain
words ("des vies infinies", "remets mon énergie au maximum") and you go and
find the address, by playing the game and watching memory. When you have it,
you hand it over with `cheat-add` and it survives into the windowed session the
player is actually using.

---

## What makes this possible

Three properties of this emulator, and the method rests on all three:

* **the whole of WRAM is readable** — `read-mem` reaches `7E:0000`–`7F:FFFF`,
  128 KB, without disturbing the console;
* **save states are complete and instant** — `save-state` / `load-state` put the
  console back exactly where it was, so the same event can be replayed;
* **emulation is byte-identical on replay** — the same state plus the same
  inputs always produces the same frame. A round of the search is reproducible,
  which is what lets you *intersect* rounds instead of guessing.

Observation never advances the console: `read-mem` and `screenshot` cost zero
frames. Only `step` and `press` move time forward.

---

## The method: successive intersection

The value the player cares about is one byte (sometimes two) somewhere in
128 KB. You do not look for it — you eliminate everything that is not it.

1. Take a full snapshot of WRAM.
2. Make the event happen (lose a life, collect a coin, take a hit).
3. Snapshot again, and keep only the addresses that moved **the way the counter
   must have moved** — down by exactly one, up by exactly one, up by exactly ten.
4. Repeat. Each round divides the survivors by one or two orders of magnitude.

Three to five rounds is the normal range. In practice: 131 072 → ~12 → ~5 → 3,
and it stops going down because what is left is the counter *and its relatives*
(see "Telling a real hit from a coincidence" below).

Two rules make the difference between converging and flailing:

* **Predicate, not "changed".** "This byte changed" keeps thousands of
  addresses. "This byte is exactly one less than it was" keeps a dozen on the
  very first round. Always state the arithmetic.
* **Same event, every round.** Replay the *same* input sequence so the rounds
  differ only in the counter's value. Save a state at the point the event
  starts, so you can come back to it.

---

## Driving it

Everything below is one JSON object per line on the process's stdin, one
response per line on its stdout. Start the emulator with:

```
prisme "roms/<game>.zip" --agent [--load-state <a .state or .resume file>]
```

### Get to the event

The search only works on a running game, so first drive the menus. `press`
holds buttons for N frames, then releases for M:

```json
{"cmd":"press","button":"Start","frames":6,"release":90}
{"cmd":"screenshot"}
```

Screenshot often and **look at the pictures**. Menus, intros and story text
swallow input for hundreds of frames; a sequence that "does nothing" is almost
always a screen you have not identified. When you reach the situation the event
happens in, freeze it:

```json
{"cmd":"save-state","path":"/tmp/level.state"}
```

### Snapshot WRAM

`read-mem` caps at 4096 bytes per request, so a full snapshot is 32 requests:

```json
{"cmd":"read-mem","addr":"7E:0000","len":4096}
{"cmd":"read-mem","addr":"7E:1000","len":4096}
…
{"cmd":"read-mem","addr":"7F:F000","len":4096}
```

The response carries `hex`. Concatenate the 32 answers and you have a 128 KB
buffer indexed from `7E:0000`. Bank `7E` alone (16 requests) is enough for most
games, but the second bank costs little and a game that keeps its counters
in `7F` is not rare enough to gamble on.

Drive this from a small script rather than by hand — you will take five or six
snapshots.

### One round

```
run the event   (press … / step …)
snapshot
survivors = { a ∈ survivors : new[a] == old[a] - 1 }
```

Print the count and the first dozen addresses after every round. A round that
does not shrink the set means the event did not happen — check the screenshot
before running another one.

### The control round

The event rounds keep everything that moves *with* the event. They also keep
everything that moves *anyway*: frame counters, animation timers, RNG state.
So run one round where **nothing happens** — 240 frames of `step` with no
input — and require the survivors to be *unchanged*.

> Run the control from a **settled** moment. On the search transcribed below,
> the control was run while a death animation was still resolving; the game
> went on to take the last life during those "idle" frames and the control
> eliminated the very address being looked for. If the value moves during the
> control, screenshot first and ask whether the game was really idle.

---

## Telling a real hit from a coincidence

After four rounds you will have two to five addresses, not one. They are not
all wrong, and they are not all the counter. The usual company:

* **the display mirror.** Games copy the counter into a status-bar tile buffer
  every frame. It follows the counter exactly, so no amount of intersecting
  will separate them. Freezing it freezes the *number on screen* while the game
  keeps taking your lives — the worst possible failure, because it looks like
  it works.
* **the saved copy.** A second copy kept for the overworld / the save file /
  the other player. It tracks the counter but the running level does not read
  it. Freezing it changes nothing at all.
* **a frame or animation counter** that happens to have ticked down once per
  round. The control round removes these.
* **the low byte of a 16-bit value.** Harmless — write both bytes.

**The test that settles it is a write, not another round.** Take the state
back, write a conspicuous value to one candidate, step a frame or two, and look
at the screen:

```json
{"cmd":"load-state","path":"/tmp/level.state"}
{"cmd":"write-mem","addr":"7E:0DBE","hex":"63"}
{"cmd":"step","frames":4}
{"cmd":"screenshot"}
```

* the number on screen changes → this address **feeds the display**;
* the screen does not move → it is a copy the game does not read here.

That splits the candidates into "the counter or its mirror" and "everything
else". To split the last pair, freeze one and **let the event happen again**: a
real counter holds and the player stops losing lives; a display mirror shows a
frozen number while the game carries on. That is the only conclusive test, and
it is cheap — one more round.

Note the offset while you are there. `MARIO ×5` with `7E:0DBE = 04` means the
game stores *lives minus one*; the value to write for 99 lives is `62`, not
`63`. Read the screen, not your assumption.

---

## Handing the result over

```json
{"cmd":"cheat-add","name":"Vies infinies","addr":"7E:0DBE","hex":"63","kind":"freeze"}
```

| field | meaning |
| --- | --- |
| `name` | what the player will read on the game sheet. Also the cheat's identity: adding the same name again **replaces** it, which is what a re-run of the search wants. |
| `addr` | `BB:AAAA`, or bare hex (`7E0DBE`). The MMIO window `$2000-$5FFF` of the system banks is refused. |
| `hex` / `bytes` | the payload, 1 to 64 bytes, written to consecutive addresses. |
| `kind` | `freeze` (rewritten after every frame) or `once` (written a single time). Defaults to `freeze`. |
| `enabled` | defaults to `true`. |

**Choose `freeze` for anything the game decrements** — lives, time, ammunition,
energy that drains. A value set once is taken straight back by the game's own
logic; that is the whole reason `freeze` exists. Choose `once` for something the
game does *not* rewrite by itself: a stock of money, an item flag, a bar you
want refilled now.

The response echoes the whole list and names the file it was written to:

```json
{"ok":true,"cheat":{"name":"Vies infinies","addr":"7E:0DBE","hex":"63","kind":"freeze","enabled":true},
 "replaced":false,"count":1,"path":"…/<game>.cheats.json","id":7}
```

That file is `<game>.cheats.json`, beside the game's `.srm` and `.state` (or in
the configured save folder, under the game's id). The windowed application reads
it when the game is loaded, so the cheat found in a headless session is in force
the next time the player presses `Jouer`, and appears on the game sheet where
they can switch it off.

The three other commands:

```json
{"cmd":"cheat-list"}
{"cmd":"cheat-enable","name":"Vies infinies","enabled":false}
{"cmd":"cheat-remove","name":"Vies infinies"}
```

Frozen cheats are applied on the agent channel too, after every emulated frame,
so you can verify your own work before telling the player it is done:

```json
{"cmd":"press","buttons":["Right","B"],"frames":800}
{"cmd":"read-mem","addr":"7E:0DBE","len":1}
{"cmd":"screenshot"}
```

Read the screenshot. The number on the HUD is the proof; the memory read alone
is not, because you have just proved that a mirror can hold too.

---

## A worked example, run end to end

Super Mario World (in `Super Mario All-Stars + Super Mario World (E) [!].zip`),
looking for the lives counter. Getting to a level took the longest: the
title screen, the file select, then a story message that holds the game for
~2500 frames and only ends on a button press. From the overworld, one `Right`
and one `A` enter the first level, with `MARIO ×5` in the HUD.

The event: hold `Right`+`B` and jump — 816 frames later Mario walks into a
Koopa and the HUD reads `×4`. Replaying the same 816 frames kills him again,
and again, so the rounds are `5→4`, `4→3`, `3→2`, `2→1`.

Predicate: `new[a] == old[a] - 1`.

| round | survivors |
| --- | --- |
| start | 131 072 |
| death 1 | 12 |
| death 2 | 5 |
| death 3 | 3 |
| death 4 | 3 — `7E:0DB4`, `7E:0DBE`, `7E:0F17` |

Three left, and no further round will separate them. The write test does:

* `7E:0DBE` ← `63`, step 4 frames: the HUD reads `MARIO ×99`. **This one.**
* `7E:0DB4` ← `63`: the HUD does not move — the saved copy.
* `7E:0F17`: the status-bar tile that draws the digit — a mirror.

`7E:0DBE` held `04` while the HUD showed `×5`, so the game stores lives minus
one and `63` is 99 lives. Handed over with:

```json
{"cmd":"cheat-add","name":"Vies infinies","addr":"7E:0DBE","hex":"63","kind":"freeze"}
```

and verified by dying four more times with the cheat on: the HUD still reads
`×99`.

---

## Checklist

- [ ] Reached the situation the event happens in, and saved a state there.
- [ ] The event is a fixed input sequence you can replay verbatim.
- [ ] Each round used an arithmetic predicate, not "changed".
- [ ] Ran three to five rounds and watched the count fall.
- [ ] Ran a control round from a settled moment.
- [ ] Wrote to each survivor and looked at the screen.
- [ ] Froze the winner and let the event happen again.
- [ ] `cheat-add` with a name the player will understand, and `freeze` unless
      you know the game leaves the value alone.
