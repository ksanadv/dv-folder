# dvfiles

A file manager in the spirit of Windows 10's File Explorer, written in Rust on plain Win32 and GDI: browse, search,
copy, move, rename, recycle and delete, with Explorer's keys, menus and Properties, plus tabs, a preview pane and
thumbnails. One small exe, no runtime, no admin rights. A folder window is on screen in about 60 ms and holds 7 MB
where Explorer's takes 130 MB, idles at 0% CPU, searches a whole drive in under a second through its own index, makes
photo thumbnails four to five times faster than Windows with its own decoders and cache, and copies many files,
deletes and recycles 5 to 16 times faster than the Windows shell's engine.

## Install

Run `dvfiles-<version>-windows-x64-setup.exe`. It installs for the current user into
`%LOCALAPPDATA%\Programs\dvfiles`, and its window has a box for each extra: a Start menu shortcut, a desktop
shortcut, `dvfiles-cli` on your Path, "Open in dvfiles" on every folder's and drive's menu, and opening folders with
dvfiles instead of File Explorer (off unless you tick it; **... > Open folders with dvfiles** switches it later too: it
sets the default verb for folders and drives under HKCU and puts the previous one back when turned off). Running it
again updates in place and starts from what is installed; `--quiet` does that with no window. Uninstall from Apps &
features. The `-portable.exe` is the same program without the installer; run with `--default on|off` it registers
nothing else.

```powershell
cp .cargo\config.toml.example .cargo\config.toml   # once; set target-dir for your machine
.\build-installer.ps1                              # dist\dvfiles-<version>-windows-x64-setup.exe, -portable.exe, -cli.exe
```

## Use

It works like Explorer: double-click to open, Backspace or Alt+Left for back, Alt+Up for the parent, F2 to rename,
Del to recycle, Shift+Del to delete for good, Ctrl+C / Ctrl+X / Ctrl+V, Ctrl+Shift+N for a new folder, F5, Ctrl+L or
Alt+D for the address bar, Ctrl+F or F3 for search, Ctrl+Z to undo, Ctrl+N for a new window, Ctrl+T for a new tab,
Ctrl+W to close it, Ctrl+Tab and Ctrl+1..9 to switch tabs, a middle click on a folder to open it in a tab behind,
Alt+P for the preview pane, Alt+Enter for Properties, type a name's first letters to jump to it, drag a band to
select, Ctrl+wheel to switch between Details and Large icons.

| part | what it has |
|---|---|
| address bar | breadcrumbs with a folder menu on each chevron; click the empty part to type a path, `%VARIABLES%`, `..`, or a few letters of a folder you have been to ("dl", "proj api"): it jumps to the best match by how often and how lately you went there, as zoxide does; else anything Windows can run |
| search | the box searches below the shown folder while you type, results stream in with their folder; `*` and `?`, `"exact words"`, `-exclude`, `ext:pdf;docx`, `type:folder`, `size:>10MB`, `size:1MB..1GB`, `modified:24h` / `7d` / `2w` / `today`. ... > Changed recently (Ctrl+Shift+R) lists the files changed in the last week below the folder, newest first. Typing more of a word filters the finished results instantly instead of searching again; a single letter waits for Enter; a search stops at 100,000 matches. On NTFS drives it goes through dvfiles' own index (see below; ... > Fast search index turns it off) |
| tabs | several folders in one window; the strip appears above the toolbar only once there are two, so a single folder looks as before. A tab in the background keeps where it was (history, selection, scroll), not its listing |
| preview pane | Alt+P or View > Preview pane, laid out as Windows' Details pane: the item's picture across the top (a photo or video as wide as the pane, PDFs, cover art, a text or code file's first lines, else its icon), its name, its details (type, size, location, date, a picture's dimensions, a song's or video's length, a program's version) and a Properties button. With nothing selected it shows the folder itself; several items show their count and size. The left edge drags wider or narrower |
| Large icons | 96 px pictures: photos (JPEG, PNG, GIF, BMP) decoded by dvfiles itself, videos, PDFs and other kinds from Windows; every thumbnail kept in dvfiles' own cache, so a folder seen before shows its pictures at once; programs' and shortcuts' own icons, and dvfiles' icons for everything else |
| icons | dvfiles draws its own, every one, the toolbar's too: shapes in the spirit of Windows 10's with a little depth (a gentle bevel, a soft shadow, a light gloss on the coloured marks); folders as Windows 10 draws them, the darker back and tab, a white sheet, the lighter front; the user's folders carry their emblem (Downloads' blue arrow, Pictures' photo, Music's note, ...); drives, This PC and Quick access have theirs; every kind of file is a page with a curled corner, a glossy mark for its kind and the type in grey letters ("WAV", "PDF"), the mark drawn larger at small sizes so kinds stay apart in Details. Programs, icon files and shortcuts keep their own icon, read from the file; a shortcut gets Windows 10's flat arrow in a white square, and its ".lnk" is never shown, as in Explorer. The toolbar and the small marks are one set of thin line icons in the text's colour |
| command bar | New, Cut, Copy, Paste, Rename, Delete, Sort, View, Properties and a ... menu (select all / none / invert, copy path, terminal here, new window, theme, default for folders) |
| right-click | Open, Open in new window, Open with, Run as administrator, Cut, Copy, Paste into folder, Copy as path, Pin to Quick access, Open in Terminal, Delete, Rename, Properties, and **Show more options** for the shell's full menu (7-Zip, WinRAR, Send to, ...), drawn as dvfiles' own menu with the pictures the programs give their items |
| menus and questions | dvfiles draws its own menus (the right-click menus, New, Sort, View, ..., the address bar's folder lists, and the text boxes' Cut/Copy/Paste menu) and its own message boxes, in the window's dark or light look: rounded, with rounded highlights. Keys work as in Windows' menus (arrows, Enter, Esc, Right/Left for submenus, a letter jumps to the items starting with it), long menus scroll, submenus open after Windows' menu delay. A question such as "permanently delete?" defaults to No, as Windows' box did. Show more options is drawn the same way, from the shell's own items (what 7-Zip, WinRAR and the like add included); Windows' menu appears only if a program's item draws itself with no text |
| Open with | dvfiles' own window: the programs Windows lists for the type (Store apps included), recommended ones first with the default marked, another program by its path, and a link to Windows' Default apps for "always" (only Windows' Settings may change a default). A file whose type has no program opens it too, in place of Windows' "How do you want to open this file?" |
| copy, move, delete | the progress window shows Windows 10s green speed graph (smooth, antialiased, with the current speed marked), time and items left, Pause and Cancel; name conflicts are answered in the same window: replace, skip or keep both, once or for all. Copies (and moves to another drive) started while one runs wait their turn, so the disk does one thing at full speed (Settings: Copy one at a time) |
| folder sizes | View > Folder sizes fills the Size column for folders too (Explorer leaves it empty): each subfolder is measured in the background, visible ones first, metadata only (cloud files are not downloaded); sizes are kept for the session and F5 measures again |
| undo | After a rename, move, copy or delete a small card at the bottom of the list says what happened, with Undo, for six seconds. Ctrl+Z (or ... > Undo) puts back renames, new items, moves on one drive, copies, and deletes to the Recycle Bin (restored instantly from dvfiles' own bin records). It checks first that nothing is in the way and reports what it could not put back. Jobs that replaced or merged items, moves between drives and permanent deletes are not offered |
| everyday | **Rename several at once**: select them, F2, type `Holiday #` (numbers), `img_###` (001, 002...), `2026 *` (keeps each name), `old/new` (replaces text in every name), `:lower`, `:upper`, `:title` or a plain name (numbered as Explorer does); the old and new names are shown before anything changes, and one Ctrl+Z puts them back. **Paste a picture or text as a file**: Ctrl+V with a screenshot on the clipboard makes a PNG, a web address a shortcut, other text a .txt. **Shelf**: right-click > Add to shelf, or drop items on Shelf at the bottom of the left pane, gathers them from anywhere; a click on Shelf (or ... > Shelf) copies or moves them all into the folder shown. **Folders open as you drag over them**, in the list and the left pane. **New downloads**: in Downloads, what arrived since you last looked has a dot, and right-click > Put away moves files to Pictures, Music, Videos or Documents by their kind. **Photo viewer**: a picture opens in dvfiles' own viewer, Left/Right through the folder (the next one is decoded ahead), Ctrl+wheel or +/- to zoom and drag to move, R to turn, Delete to recycle, F11 full screen, Esc closes. Each can be turned off in Settings (Ctrl+,) |
| Properties | three tabs, as Windows': General (type, opens with, location, size and size on disk, contents counted live, dates, read-only and hidden; drives get a usage ring), Details (a program's description, versions, company and copyright from its version resource) and Security (the owner, and who may do what, read-only). Changing permissions and Previous versions open Windows' own sheet |
| clipboard and drag and drop | the formats Explorer uses (`CF_HDROP`, with "Preferred DropEffect" on the clipboard), so cut, copy, paste and dragging work with Explorer and other programs. A drag out starts at once whatever the selection, its picture drawn as Windows 10 draws its own: the item's picture held at its bottom middle and, for several, their count in a small blue square in its middle. Folders can be dragged out of the navigation pane too, as from Windows' tree (a folder there opens when the button is let go). Dragging over the list or the navigation pane lights up the folder a drop would go into and the drag image says "Move to Documents" or "Copy to Documents"; Ctrl copies, Shift moves, otherwise a move on the same drive and a copy to another, as in Explorer |
| look | a frame (title bar, toolbar, left pane, status bar) with the content on it as a raised card with rounded corners; rounded buttons, boxes, rows and menus; your Windows accent colour for the selection, focus and the New button. **Glass** (Settings): the frame shows what is really behind the window, a browser or a video, blurred and darkened by dvfiles itself (never tinted by it); Windows leaves dvfiles out of screenshots and screen sharing while it is on. Dark or light follows Windows or is set in Settings; while a folder loads or a search runs the address bar fills with green; F2's rename box sits where the name is |

`dvfiles --copy <items> --to <folder>` and `--move` run a job with the progress window from a script or shortcut.

## Compared with File Explorer

Windows 11 build 26300, Ryzen 7 7435HS, NVMe SSD, display at 125%, release build. A start is timed from launch until
a visible, uncloaked window exists. dvfiles' starts, memory, idle and thumbnails were measured again with this build
on 15 October 2026, its windows kept from taking the keyboard (`DVFILES_NOACTIVATE=1`, which acts after the window
shows); Explorer's numbers, and dvfiles' 100,000-file folder, search and file jobs, come from earlier builds on the
same machine and Windows build.

### Windows, memory and CPU

| | File Explorer | dvfiles |
|---|---:|---:|
| new window showing `C:\Windows` | 793-1,056 ms | 54-67 ms |
| new window showing `C:\Windows\System32` (4,825 items) | 792-1,099 ms | 58-60 ms |
| new window on a folder of 100,000 files | 968 ms until the window appears; the list keeps filling after that | 176 ms with the complete, sorted list |
| a window on System32, 6 s after opening: private memory | 129-130 MB | 6.8-7.2 MB |
| the same: working set | 187-190 MB | 19-22 MB |
| the same: CPU time used | 3.2-3.3 s | 0.08-0.13 s |
| search `dll` in System32: private memory, working set, CPU | 53-54 MB, 78 MB, 0.55 s \*\* | 27 MB, 46 MB, 0.23-0.5 s |
| idle, 8 s after the last input: memory | not measured | 3.0-3.3 MB private, 0.1 MB private working set |
| idle: CPU over 10 s | not measured | 0 ms, with Glass on too (it looks behind the window on a thread every 4 s while nothing there changes, twice a second while it does, and not while other windows cover it) |

Explorer's starts are 7 runs; dvfiles' the middle 7 of 11 (medians 59 and 60 ms). The first start after dvfiles' exe
was replaced took 382 ms, while the antivirus read the new file. On this Windows build every Explorer window opened by
`explorer.exe <folder>` runs in its own process, so each app's memory and CPU are that window's whole process (3 runs
each, the same moment after opening). \*\* Explorer hands the search to the Windows Search service, whose processes
are not counted here.

### Thumbnails

A Large icons window (1,100 x 700) on 204 photos copied fresh from Windows' wallpapers (JPEG, up to 3840 x 2160),
timed from the window showing until every thumbnail it asked for (the screen, and one screen above and below) had
arrived, from dvfiles' trace. "Windows" is the thumbnail maker File Explorer uses (`IShellItemImageFactory`), called
by dvfiles in place of its own, in alternating runs. Explorer's own window gives no sign of when its last thumbnail is
in, so it was not timed.

| | Windows | dvfiles |
|---|---:|---:|
| new photos, first window | 1,197-1,248 ms | 256-288 ms |
| the same photos, a second window | 170-175 ms, from Windows' cache | 5-26 ms, from dvfiles' cache |
| photos Windows had already made thumbnails of, dvfiles' cache empty | 159-191 ms, from Windows' cache | 154-191 ms, decoded by dvfiles |
| private memory afterwards | 9.6-11 MB | 6.6-7.3 MB |
| one new photo | 68 ms | 35 ms |
| a 120 px thumbnail as stored | about 3.7 KB (JPEG) | about 3.7 KB (JPEG, dvfiles' encoder) |

The photos were on an NVMe SSD and in memory. On a network share or a spinning, removable or optical disk, dvfiles
asks Windows' cache first, as reading whole photos there could take longer than Windows' finished thumbnails; that was
reasoned, not measured. Decoding is held to 128 MB at once: 16 4K PNGs (decoded whole before shrinking) peaked at
81 MB.

### Parts dvfiles replaced, against the Windows call it used before

| | Windows | dvfiles |
|---|---:|---:|
| a right-click menu opened and closed with Esc (median of 15) | 19-20 ms (`TrackPopupMenuEx`) | 10.4 ms |
| a program's icon | 1.2 ms (`SHGetFileInfo`) | 0.34 ms, read from the file |
| a drag of 1,000 files starts | after 1 s (the shell's data object) | at once (5,000 paths in 4 ms) |
| a drag of System32's 4,825 files starts | after 7.6 s | at once |
| a name in the list drawn | 57 us (`DrawTextW`) | 18 us (`ExtTextOutW`, also Windows; the same pixels) |

### File jobs

| | the shell's engine | dvfiles |
|---|---:|---:|
| copy 2,000 small files in 20 folders (20 MB) | 4,807 ms | 957 ms |
| copy one 512 MB file | 357 ms | 369-420 ms (same; one file is limited by the disk) |
| delete those 2,000 files permanently | 2,297 ms | 152 ms |
| delete one folder of 20,000 files permanently | 22.5 s | 2.3 s |
| move a folder of 2,000 files to the Recycle Bin | 1,517 ms | 94 ms |
| move that folder to another folder on the same drive | 8 ms | 29 ms (both one rename; dvfiles' figure includes starting `dvfiles-cli`, about 13 ms) |

The median of 3 alternating runs on a warm cache. They compare dvfiles' engine (`dvfiles-cli`) with the Windows
shell's engine called through `SHFileOperation` (which runs Explorer's `IFileOperation` engine) without a progress
window, not with Explorer's own window.

### Search

Warm cache, for `notepad` anywhere in the name (for all of C:, `report`). Explorer's own search box was not timed; the
Windows Search index can answer indexed locations faster than any walk, so dvfiles is compared with the two walkers
Windows ships. "dvfiles index" is the whole run of `dvfiles-cli index` (start, load, catch up with the journal, match,
read the matches' folders); the window shows the same in its status bar.

| | `dir /s /b` | `Get-ChildItem -Recurse` | dvfiles walking | dvfiles index |
|---|---:|---:|---:|---:|
| `C:\Program Files` (152,361 items) | 2.8 s | 6.1 s | 0.23 s | 0.17 s |
| the user profile (1.2 million items) | 28 s | 59 s | 2.2-4.5 s (15.6 s on a cold cache) | 0.24 s |
| all of C: (2.1 million items, 368,000 folders) | | | 7.9-10.7 s | 0.26 s; 0.65 s in the window, 2,961 results |

### Features side by side

| | File Explorer | dvfiles |
|---|---|---|
| tabs, preview pane, Details and Large icons | yes | yes |
| folder sizes in the Size column | no | yes (View > Folder sizes) |
| search | names, and contents where Windows Search indexes them | names only, through its own index (no admin rights) or a walk; `ext:`, `size:`, `type:`, `"exact"`, `-exclude` |
| undo | renames, new items, moves, copies, recycles | the same, except moves between drives and jobs that replaced or merged items |
| name conflicts while copying | a separate dialog | in the progress window |
| right-click menu | Windows 11's menu, and "Show more options" for the classic one | dvfiles' menu; Show more options drawn by dvfiles with the shell's items |
| Open with | Windows' dialog | dvfiles' window with Windows' list of programs |
| Properties | the shell's sheet, with the tabs programs add | General, Details, Security (read-only); Windows' sheet for permissions and Previous versions |
| zip files | browsed as folders | opened in their program |
| grouping | yes | no |
| Home, Gallery, Libraries, Recycle Bin, Control Panel | yes | no |
| network | browsing the network | typed `\\server\share` paths |
| Win+E and the taskbar button | yes | no, they open Explorer |

Not timed: Explorer's own window making thumbnails; thumbnails on network shares and spinning or removable disks;
dvfiles' message boxes, Open with and Properties against Windows'; Explorer when idle. The tests open Open with and the
shell's menu commands and cancel them, so no program is started from them.

## dvfiles' own code, and what Windows still does

Plain Win32 calls (reading folders and files, GDI drawing) are not counted here; this is about Windows' ready-made
parts: the shell, its dialogs and controls, its codecs.

| part | dvfiles' own | still Windows' | why |
|---|---|---|---|
| window, list, address bar, tabs, preview pane | all of it, drawn by dvfiles | the address, search and rename boxes are Windows' edit controls | input methods, selection, undo and screen readers |
| icons | every icon, drawn by code; shortcuts read from the .lnk file | programs' own icons read from the file; type names ("Text Document") from the shell | type names follow what programs register: 176 of 1,058 extensions came out different read from the registry |
| menus and message boxes | all of them | Windows' menu only for a program's item that draws itself with no text | |
| Show more options | the menu, drawn as dvfiles' | its items and their commands (`IContextMenu`) | that is how 7-Zip, WinRAR and the like add theirs |
| Open with | the window | the list of programs, starting them, and changing a default (Settings) | only Windows may change a default |
| opening a file | the question when a type has no program | `ShellExecute` | Windows' choice of program (the registry matched it for 301 of 390 types), SmartScreen, the downloaded-file warning |
| Properties | General, Details, Security | editing permissions, Previous versions | |
| pictures and thumbnails | JPEG, PNG, GIF and BMP decoders, a JPEG encoder, the thumbnail cache | thumbnails of videos, PDFs, RAW and HEIC; Windows' cache asked first on slow drives | codecs Windows already has |
| media details | lengths, tags and formats of MP3, MP4, FLAC, Ogg, MKV, AVI, ASF and WAV | | |
| search | the walker and the index (NTFS's change journal) | | |
| copy and move | the engine, its threads and the progress window | `CopyFile2` for each file | the antivirus scans `CopyFile2` copies more cheaply than an own copy loop |
| Recycle Bin | moving items in and writing the bin's records | the shell for cloud, removable and network drives, and items too large for the bin | |
| drag and drop | the data object and the drop target | the drag image helper | |
| dates | | `GetDateFormatEx`, `GetTimeFormatEx` | every locale's calendars, eras and digits |

## How it is fast

- **Folders** are read 64 KB at a time with `GetFileInformationByHandleEx`, hundreds of entries a call and never a
  per-file open; only the rows on screen are drawn.
- **Search** walks folders on several threads and keeps its own index, caught up from NTFS's change journal, which any
  user may read.
- **Thumbnails** are decoded by dvfiles on four threads and kept in its own cache, with none of Windows' thumbnail code
  loaded for photos.
- **File jobs** copy up to 8 files at a time with `CopyFile2`, recycle by writing the Recycle Bin's own records and
  delete on 4 threads.
- **Menus, message boxes, icons and dragging** are dvfiles' own, each faster than the Windows part it replaced.
- **Start**: the window is drawn before it is shown, while the folder is read on a thread: 37-38 ms from `main` to a
  visible window, after the 16 ms Windows takes to start any process.
- **Idle**: no polling and no timers; 8 s after the last input the bitmaps are freed and the working set handed back.

How each part works, with its measurements, and what was tried and left out: [performance-notes.md](performance-notes.md).

## For AI agents and scripts: dvfiles-cli

```text
dvfiles-cli ls <folder> [--json]
dvfiles-cli search <folder> <query...> [--limit N] [--json]
dvfiles-cli size <path...> [--json]
dvfiles-cli copy|move <source...> <destination folder> [--replace | --skip | --keep-both]
dvfiles-cli recycle <path...>
dvfiles-cli delete <path...> --yes
dvfiles-cli index <drive> [folder] [query...] [--rebuild]   the search index: build, catch up, search, with timings
```

## Limits

- No grouping, no zip folders (they open in their own program), no network browsing (`\\server\share` paths work
  when typed).
- Search matches names only, not contents, and does not use the Windows Search index. Drives without a change
  journal (FAT, exFAT, network) are always walked.
- Libraries, the Recycle Bin, Control Panel and other shell-only folders are not shown; use Explorer for those.
- Drag and drop was tested without a mouse, through the targets' own calls: dvfiles' drop target with dvfiles'
  data object (`DVFILES_DNDTEST`) and Explorer's drop target for a folder (`DVFILES_SHELLTEST=1 cargo test`). Drags
  onto the Recycle Bin and into browsers and mail programs were not tried; they read the same `CF_HDROP`.
- Folders in the navigation pane are read when expanded; a slow network drive there makes the window wait.
- Thumbnails on network shares and spinning, removable or optical disks still ask Windows' cache first; that order was
  reasoned, not measured.
- Win+E and the taskbar's File Explorer button still open Explorer.
- dvfiles' menus and message boxes tell screen readers only the basics: the menu's start and end, and the lit item's
  name as the menu's name when it changes. Windows' own menus describe themselves fully.
- Tested on one 125% monitor on Windows 11; on Windows 10 the title bar keeps Windows' own colour.

Settings live in `%APPDATA%\dvfiles\settings.txt`. `cargo test` runs the unit tests (sorting, sizes, names, queries,
selection, Recycle Bin records, listing, walking, copy/move/delete, a real round trip through the Recycle Bin, the
picture decoders and encoder, media details, the thumbnail cache, menus). `DVFILES_SHELLTEST=1 cargo test` also
checks against Windows itself: its picture decoders and media properties on sample files, and Explorer's drop target.
Set `DVFILES_TRACE=1` to log input events, menu choices and the start's timings to `%TEMP%\dvfiles-trace.txt`;
`DVFILES_NOACTIVATE=1` keeps dvfiles' windows from taking the keyboard, for tests run while someone works at the
machine.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). The parallel walk follows the design of WinMem's walker (MIT).
