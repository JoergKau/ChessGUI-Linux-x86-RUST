# Asset licensing

The asset tree in this folder (`public/images/board/`, `public/piece/`,
`public/font/lichess-chess.ttf`, `public/data/openings/`) is a curated copy
of the public asset tree of [lichess](https://lichess.org), taken from the
[`lichess-org/lifat`](https://github.com/lichess-org/lifat) repository (with
`.orig`/`.thumbnail` variants and the horsey board-side overlay images
removed — SiliconBoard never reads them). See that repository for the
authoritative, per-asset licence list.

- **Piece sets** (`public/piece/<set>/twelve SVGs`) come from their
  respective authors unter CC-BY-SA-type licences, e.g. the default
  `cburnett` set by Colin M. L. Burnett (CC-BY-SA 3.0). Different sets have
  different authors; the upstream repository documents each one.
- **Board images** (`public/images/board/`) originate from lichess and other
  authors, again under their upstream per-image terms.
- **Figurine font** (`public/font/lichess-chess.ttf`) is lichess's
  figurine font, used here only for on-screen SAN letters.
- **ECO table** (`public/data/openings/openings.tsv`) is CC0 — see
  `public/data/openings/README.md`.

The application source code, the icon in `assets/icon/` and everything not
inside this `public/` folder are MIT-licensed (see
[`LICENSE`](../LICENSE)).
