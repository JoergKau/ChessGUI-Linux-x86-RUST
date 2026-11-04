# Opening names

`openings.tsv` is the ECO opening table from
[lichess-org/chess-openings](https://github.com/lichess-org/chess-openings),
used under CC0. Columns: `eco`, `name`, `pgn`.

SiliconBoard replays each PGN line and maps the final position to the name.
The move counters are ignored when matching, so the name follows the board
position rather than the half-move number.
