# Timeperiods against the reference

`periods.cfg` holds 56 timeperiods written for this project, one for each
form of line the Naemon documentation gives and for the cases where the
forms meet. `gen` asks the reference about each of them at a few hundred
moments, with its `$ISVALIDTIME:<period>:<moment>$` and
`$NEXTVALIDTIME:<period>:<moment>$` macros, and writes the answers to
`expected.tsv`: 27,496 lines of

    <period> <moment> <valid> <next>

`crates/checkd-core/tests/timeperiods.rs` asks checkd's evaluator the same
questions and wants the same answers, with the exceptions below.

The reference's clock is UTC in these runs. Nothing here tests another
time zone or a change to or from summer time.

## Where checkd does not follow the reference

In these cases the reference does something its own documentation does
not say, and that a person who writes the line would not expect. checkd
goes by the documentation. The test names the periods concerned
(`ON_PURPOSE`) and fails if one of them comes to agree with the reference,
so that this list stays true. The second test in the file,
`by_the_documentation`, holds what checkd answers for them.

A config that uses one of these forms is checked and notified at other
times under checkd than under the reference. For the first four, `checkd
run` gives a warning at the start that names the period and the line. It
gives none for the fifth, which cannot be seen from one period alone, or
for the sixth.

1. **A span with a skip.** `day 1 - 15 / 5` should be every fifth day from
   the 1st to the 15th. The reference takes no notice of the end of the
   span once a skip is given: the days go on to the end of the month, or
   for a span of calendar dates without end. checkd ends the span where
   it says. Periods `cal-span-skip`, `day-span-skip`, `md-span-skip`,
   `wd-span-skip`, `wom-span-skip`.

2. **A day counted from the end of the month.** `day -1` should be the
   last day of every month, and `february -1` the last day of February.
   The reference counts back from the 31st whatever the month, so `day -1`
   is valid only in months of 31 days and `february -1` never. checkd
   counts from the month's real last day. Periods `day-last`,
   `day-negative-span`, `md-last`.

3. **A span of month dates over the turn of the year.** `december 20 -
   january 5` is, to the reference, over on 31 December. checkd carries it
   on to 5 January. Period `md-span-wrap`.

4. **A span that ends after the month does.** `february 20 - 30` runs, in
   the reference, to the end of the year. checkd ends it with February.
   Period `md-span-past-end`.

5. **A period with nothing left.** Where the excluded periods take away
   every valid time, the reference still names a next valid time, at which
   the period is not valid. checkd says there is none. Period
   `nothing-left`.

6. **How far ahead is looked.** The reference gives up looking for the
   next valid time somewhere near 300 days ahead. checkd looks 366 days
   ahead. The test lets a `never` of the reference stand against an answer
   of checkd's that lies beyond 289 days.

These were decided while the evaluator was written and have not been put
to anyone who runs a config with such lines. Each could instead be copied
from the reference, as the loader does with the reference's quirks in
reading a config.

## Not looked at

- Time zones and summer time, as said above.
- `day 1 / 5`, a single day of the month with a skip.
- A span with `/ 1`.
- Very long lines. A weekday line with 720 ranges, written for a probe,
  made the reference stop while it read its configuration, with
  `*** stack smashing detected ***` and a core dump. checkd was not tried
  on it.
- A span that begins on a day its month does not have.
- `use_timezone` and a timezone of a single host.
