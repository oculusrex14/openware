# OpenCW web shell — E2E walkthrough

Click path for accepting the web UI against a real engagement on Windows.
No screenshots: every step says what you click and exactly what must appear.

Start it (strix, Windows):

```
opencw serve <engagement-dir> [--port 8078] [--bind 127.0.0.1]
```

`serve` prints the bound URL and the exact `curl` smoke lines; open that URL in
Edge or Chrome. The UI is a single page; every navigation is `#/section`.

Vocabulary used below:

- **nav** — the fixed left rail (Dashboard, Trial Balance, Documents, Accounts,
  Entries, Signoff, Operations bay, Manifest).
- **top bar** — engagement name, its path, the health dot, `Refresh`, `Print`.
- **pane** — the main area to the right of the nav.
- **toast** — the banner that slides in at the bottom right.

---

## 1. Shell loads offline with no external requests

1. Open DevTools → Network, tick **Preserve log**, clear it.
2. Load the URL and press `F5`.

Pass:
- Requests are only `GET /`, `GET /static/style.css`, `GET /static/app.js`, and
  `GET /api/overview`. Filter by **Domain** — only the server host appears; there
  is no CDN, no font host, no analytics host.
- Top bar shows the engagement stem on the left of the health dot and the
  engagement directory path after it.
- Health dot is green with the label `HEALTHY` (red `ATTENTION` means a kernel
  table failed to decode — that is a data finding, not a UI defect).

## 2. Dashboard census matches the CLI

1. Click **Dashboard** in the nav (or press `#/dashboard` in the address bar).

Pass:
- A card row: *Engagement health*, *Posting accounts*, *AM chart rows*,
  *Grouping schemes*, *Live documents*, *Deleted documents*, *CaseView forms*,
  *Balance rows*.
- The numbers equal `opencw <dir>` (the shell dashboard) on the same directory.
- Below the cards, the *Engagement* table repeats Stem / Path / Health / Work
  area, where Work area is `<engagement-parent>/work/`.
- `Jump to` buttons navigate to the other sections.

## 3. Trial balance grid: selectors, sorting, sticky header

1. Click **Trial Balance**.
2. Set **Year** `0`, **Period** `12`, **Basis** `Actual basis (A)`, **Entity**
   empty, **Rows** `All rows`; click **Apply**.

Pass:
- A dense grid whose first two columns are `Account` and `Description`, followed
  by the period/YTD columns and (when a prior year exists) the prior and
  variance columns.
- Clicking any numeric header sorts descending, clicking it again sorts
  ascending, and the `▲` / `▼` marker follows. Clicking `Account` sorts by
  number, not by string order.
- Scroll the grid: the header row stays pinned and the `Totals` row stays
  pinned at the bottom.
- Numeric cells are right-aligned and use thousands separators with 2 decimals.

## 4. Variance highlighting is present and directional

1. Still on Trial Balance, with a prior-year comparison available (the footer
   says `comparing Y-1`).
2. Look at the two right-most columns.

Pass:
- A variance larger than 5% of the prior balance (or ≥10,000 when there is no
  prior balance) has an amber background.
- Negative variances are dark red, positive variances dark green.
- A zero variance prints as an en dash `–`, not `0.00`.

## 5. CSV export comes from the browser, not the server

1. On Trial Balance, click **Download CSV**.

Pass:
- A file `tb_<stem>_Y<year>P<period>_<basis>.csv` appears in Downloads.
- Its header row is `Account,Description,<column labels…>` and its last row is
  `Totals`.
- It reflects the current sort order, and it contains exactly the rows on
  screen (all rows when Rows = All).
- Network shows **no** new request: the blob is built locally.

## 6. Document Manager lists the documents making up the file

_(steps 6a–6e are the same section continued)_

CaseWare's Document Manager is the dynamic list of **documents**, not the `SH`
table. A document is a physical file in the engagement folder; `SH` supplies the
metadata. The view shows the union through two lenses: **Documents** (the DMS)
and **Register (SH)** (the metadata index).

1. Click **Documents**.
2. Leave the lens on **Documents**, the filter empty, class `All classes`, and
   *Show internals* unticked.

Pass:
- Card row: *Documents*, *Files on disk*, *Joined to SH*, *Unindexed*,
  *Register only*, *Ambiguous*, *Internals*.
- On the hull engagement the numbers are `178` files on disk, `188` documents,
  `35` joined, `82` unindexed, `71` register-only, `5` ambiguous, `61`
  internals. (`files` counts the folder; `documents` adds register rows that
  have no file on disk.)
- The grid columns are CW's Document Manager set, in this order: **Name**,
  **Class**, **Schedule**, **Title**, **Signoff**, **Modified**, **Size**,
  **Lock / flags**, **SH match**.
- `.DS_Store`, `._<name>` AppleDouble forks, `*.ac_` and `*.bak` are **never**
  rows — the same skip law the package writer uses.
- The left tree is the folder tree (recursive subfolders), each row carrying its
  file count; clicking a folder filters the grid to it, clicking it again
  clears the filter.

## 6a. Join law: how a file finds its `SH` row

`SH` has no filename column, so the match is a documented best-effort. Hover a
row's **SH match** cell for its tier; the tier list above the grid prints the
hit count per tier on this folder.

| Tier | Law | hull2024 hits |
|---|---|---|
| `cvid` | the `.cvw` filename tail equals `SH.CVID` (`…01200RG0.cvw`) | 9 |
| `schedule` | the file stem *is* `SH.SCHEDULE` (`0160.docx`) | 1 |
| `schedule prefix` | the stem opens with a digit-leading schedule token (`0146_Review.xlsx` → `0146 REV`) | 7 |
| `title` | the stem equals `SH.SCH_DESC` (`0139 … .docx`) | 8 |
| `title contains` | stem and title contain each other, both ≥ 8 chars (`Inventory analytics.xls`) | 10 |
| `name` | the stem equals the 2-char analysis tag (`TS.cdx`) | 3 |

Keys are normalised: NFKD-folded to ASCII, lower-cased, non-alphanumerics
removed. Heading rows are excluded (their `SCHEDULE` is a `CWGUID`), as is any
cell carrying control bytes — those are the F63 zero-fill tail, never names.

Known misses, shown in the **Where the join misses** disclosure rather than
hidden: attachments (several files sharing one row — the row is flagged
`ambiguous`), files with no token at all (`Summary.pdf`, `ss.XLS`, `*.CGF`
style files, the `.ac`/`.TVR`/`.locks` machinery), a wrong-token prefix
(`0272.01_DisclosureChecklist…docx` joins `0272`), and register rows with no
file on disk (analyses, headings, index rows).

## 6b. A `.cvw` renders in the browser

1. In the grid, click the row `2024 Hull Lift Truck Review(1148441)01200RG0.cvw`
   (its **SH match** cell reads `CVID`).
2. In the properties drawer, click **Preview sheet**.

Pass:
- A new tab opens `/api/docman/preview-cvw?path=…01200RG0.cvw`.
- The page states the form code, `kind` (`statement`), block count, numbered
  cells, the cell-text count and the SHA-256 prefix.
- One bordered table per CaseView block, headed by its group (`aEQ0`, `aEQ1`,
  …) and its extent in leadsheet coordinates (`rows 1..9 · cols A..J`); gaps
  inside the rectangle stay empty rather than being collapsed.
- System cells (no leadsheet coordinate, e.g. `@@FIRM`) are listed in a
  collapsible section below the blocks.
- **Notes** is honest about the text: these hull documents carry no
  `Form/Strings` pool, so the page says *"cell text unavailable … structure is
  shown, no text is invented"*. A document whose pool survives (the staged
  `EXPO0IS0`/`FJEIMP`) shows the pool text in the cells instead.
- A document that cannot be laid out (`…04000RG0.cvw`, `…22.cvw`, `…rm.cvw`
  carry only `Form/Layout` + `Form/Styles`) opens a page that says **"Nothing
  laid out"** and offers the raw download — no empty grid is fabricated.
- The page requests nothing off-origin: no font host, no CDN, no script.

## 6c. Open, download, properties

1. Click any row; the drawer shows **Open in new tab**, **Preview sheet** (only
   for `.cvw`), **Preview inline** (only for PDF/image) and **Download**.
2. Click **Open in new tab** on a `.docx`.

Pass:
- `/api/docman/file?path=…` answers with the file's real bytes and its declared
  MIME type (`application/pdf`, the docx/xlsx OOXML types, `image/png`, …) and a
  `Content-Disposition: inline; filename*=UTF-8''…` header, so the browser
  offers to open or save it.
- The drawer's **File** table shows file, folder, class, format, size, modified
  (UTC), MIME and full SHA-256; the **SH register** table shows Schedule,
  Title, Type, PRG/NAME, LINENO, CVID, Entity, GUID, Created, CW date and the
  flag set; the **Signoff** table shows stage, signed levels, completeness and
  next level.
- A file with no register row says so in plain words and still lists its file
  facts.
- Absolute paths, `..` traversal and directories are refused (`400`/`404`);
  only files inside the engagement are served.

## 6d. Toolbar: filter, class, internals, lens

1. Type `0102` in the filter box.
2. Set class to `PDF`, then back to `All classes`.
3. Tick **Show internals**.

Pass:
- The filter matches file name, relative path, and the joined row's title,
  schedule, CVID and GUID (case-insensitive); the grid narrows after ~260 ms
  without a reload.
- Class `PDF` keeps only `.pdf` rows. **Show internals** adds the kernel tables
  (`*.dbf`, `*.cdx`, `*.fpt`) and the engagement machinery (`.ac`, `.locks`,
  `.TVR`) — they are always *counted* in the cards, just hidden from the grid.
- The footer states the rows shown, the active folder filter, the text filter,
  the class, and the lens total.

## 6e. Register lens (the `SH` metadata index)

1. Click the lens button **Register (SH)**.

Pass:
- The census cards return: Live documents, Deleted, Headings, CaseView forms,
  Analyses, Blank LINENO, Cycles.
- The grid is the register in physical CW document order: Line, PRG, Kind,
  Schedule, Title, GUID.
- Clicking a row jumps to Signoff with that document loaded.
- On the expanded hull folder the register reports `103` live rows out of
  `664` declared, and the DMS banner says the table was repaired: the `.dbf`
  ends without its `0x1A` terminator and rows past 603 are zero-fill synthesis.
  The repair is in memory only; nothing on disk is rewritten.

## 7. Account dialog shows the 12 properties in dialog order

1. Click **Accounts**.
2. Type `10` in the filter box, click an account.

Pass:
- Left list shows `AC_NO` + description; the filter matches both number and
  description.
- The right pane shows an identity panel (Type, Class, Schedule, Entity, GUID),
  then *Properties (dialog order)* with `POST, PRINT, CTL, TOTAL, SIGN, DC,
  COLUMN, DOLLAR, ULINE, PAGE, DEN, HIDDEN` in exactly that order (blank values
  print as `(blank)`).
- *Balances* shows Opening / Month / YTD / AJE / EJE / RJE right-aligned.
- Grouping assignments appear when the account carries MP nodes.
- Clicking two accounts in quick succession always ends on the second one.

## 8. Entry list and entry detail

1. Click **Entries**.
2. Leave the account filter empty; click **Load**.
3. Click each row in turn.

Pass:
- The chip after the separator reads `N entries`.
- Grid columns: Ref, Date, Type, Description, Amount, Balance, Journal, Lines.
- The Balance pill is green `balanced` or red `out of balance`; each row opens
  a detail panel showing date, description, decoded EntryType (e.g.
  `Adjusting entry (N)`), period, journal, and the account lines.
- Type a real account number in *Account* and press **Load**: the list narrows
  to entries touching that account, and the empty state says so when none do.

## 9. Signoff ladder per document

1. Click **Signoff**.
2. Pick a document in the dropdown, or go via Documents.

Pass:
- Three summary cards: Stage, Highest signed (Level *n* + role name),
  Progress *k* / 8.
- A document panel with GUID, Schedule, Stage.
- A vertical timeline with exactly eight levels in order — `Prepared by 1..3`
  then `Reviewed by 1..5`. Signed levels show a green check plus initials, user,
  date, and a signature-GUID prefix; the next unsigned level is amber; the rest
  are grey `pending`.
- A document with no signoff block shows the empty state, not an error.

## 10. Operations bay — preview before commit (every op)

1. Click **Operations bay**.
2. Confirm six cards: *Create / replace adjusting entry*, *Renumber accounts
   (cascade)*, *Year-end close*, *Expand package*, *Compress package*,
   *Backup snapshot*.
3. On each card, press `1 Preview`.

Pass for every card:
- The `2 Commit` button is **disabled** until a preview succeeds.
- The status reads `preview ready · nothing written`.
- The output shows a `preview` pill plus
- a **manifest diff table** with three counts (only before / only after /
  changed) and, per row, the file name, byte sizes, and 12-character SHA-256
  prefixes:
  - `only_a` rows on an amber background,
  - `only_b` rows on a green background,
  - `changed` rows on a violet background with *both* digests, and
- the op-specific preview:
  - entry: the balanced/unbalanced pill, difference, and the amount per line;
  - renumber: renames `from → to`, cells rewritten, per-table rows, and the
    deliberately skipped non-canonical numbers. Mode defaults to **Uniform
    delta shift** (the CLI's `--delta N` form); pick *Explicit pairs* to paste
    `FROM=TO` lines;
  - a **refused** preview is still rendered: the card shows a red `refused`
    pill with the error code, the full computed plan, and every blocking issue
    (on the corp2026 corpus a `+100` delta refuses because several accounts are
    also map codes). `2 Commit` stays disabled — nothing was written;
  - year-end: destination row counts with a delta column, the destination path,
    net income moving into retained earnings, and key totals;
  - lifecycle: source/destination plus a row-set comparison per table.
4. Now edit any field on a card that already previewed.

Pass: `2 Commit` goes disabled again and the status reads
`form changed · preview again`. Commit can never write a spec you did not see.

## 11. Operations bay — commit, audit trail, refreshed state

1. On *Create / replace adjusting entry*, fill `Ref` (`AJE-…`), `Date`
   (`YYYYMMDD`), two lines that balance (e.g. `6100` +5000 and `2100` −5000).
   The pill next to *Difference* turns green at `sum 0.00`.
2. Press `1 Preview`, then `2 Commit`.

Pass:
- The status reads `committed · preview again`, a green toast confirms, and the
  card's output switches to the `commit` pill.
- If the server returned a backup path it shows as a pill.
- **Session commits** at the bottom gains a row: time, operation, verdict
  `COMMITTED`, and detail.
- The top-bar health dot is unchanged green and the dashboard census reflects
  the (possibly unchanged) row counts after the next refresh.
- A rejected commit (unbalanced spec, missing account, stale token) shows a red
  toast with `{code,message}`, an error pane inside the card, and a **failed**
  row in the audit list.
- Try an unbalanced spec directly: the toast reads
  `Entry does not balance (difference …)` and no request is sent.

## 12. Manifest viewer + snapshot comparison + graceful errors

1. Click **Manifest**.
2. The path defaults to the engagement directory; press **Load**.

Pass:
- A file table: name, bytes, SHA-256 prefix, plus `N files · X total` beneath.
- Put a second directory (e.g. the work-area backup that step 11 created) in
  the right-hand **before/after** pair, press **Compare**.

Pass:
- The diff renders the same three-part table as the Operations bay: only in
  before, only in after, changed (with both sizes and both digests). Identical
  manifests print `No file differences between the two manifests.`
- Change the path to `/nope/does/not/exist` and press **Load**: the table shows
  `No files found at this path.` — an empty state, not a crash.
- Press `Print` (or `Ctrl+P`) on Trial Balance: the print preview drops the nav
  and the toolbar,
  keeps the top bar in black on white, and expands the grids instead of
  clipping them.

Error-state check (any time): stop the server and press **Refresh** — the top
bar flips to a red dot with `UNREACHABLE`, the pane shows the error with a
**Retry** button and the error `code`, and a red toast appears. Restart the
server, press **Retry**: everything reloads.

---

## Contract coverage

Every route in the API contract is consumed:

| Route | Where |
|---|---|
| `GET /` | shell (`index.html`) |
| `GET /api/overview` | top bar + Dashboard |
| `GET /api/tb` | Trial Balance (+ Accounts index) |
| `GET /api/docman?q=&class=&internals=&sha=&path=` | Documents (DMS grid) |
| `GET /api/docman/file?path=` | Documents → Open / Download |
| `GET /api/docman/preview-cvw?path=&format=json` | Documents → Preview sheet |
| `GET /api/docs?mode=tree\|list\|census` | Documents → Register (SH) lens |
| `GET /api/account/<ac_no>` | Accounts detail |
| `GET /api/entries?acct=` | Entries |
| `GET /api/signoff?doc=` | Signoff |
| `GET /api/manifest?path=` | Manifest |
| `POST /api/manifest/diff` | Manifest compare, Operations diff |
| `POST /api/entries/preview\|commit` | Operations card 1 |
| `POST /api/renumber/preview\|commit` | Operations card 2 |
| `POST /api/yearend/preview\|commit` | Operations card 3 |
| `POST /api/lifecycle/expand\|compress\|backup` (+ `/preview`,`/commit`) | Operations cards 4–6 |
| `GET /static/app.js`, `/static/style.css` | embedded assets |

No route is called with a method or body outside that table, and no asset is
loaded from anywhere but `/static/`.
