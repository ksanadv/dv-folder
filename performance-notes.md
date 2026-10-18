# dvfiles: how it is fast

Design notes behind the numbers in the [README](README.md): how each part works, what it measured, and what was tried
and left out. Measured on the machine the README names.

## How it works

- **Folders** are read with `GetFileInformationByHandleEx(FileFullDirectoryInfo)` into 64 KB buffers: hundreds of
  entries per call with sizes, times and attributes, and never a per-file open. Only the rows on screen are drawn.
- **Search, folder sizes and copying** walk folders on several threads at once (`src/walker.rs`), metadata only, never
  into junctions or links. Cloud placeholders are listed without being downloaded.
- **The search index** (`src/index.rs`) needs no administrator rights. The first search on a drive walks it as usual;
  afterwards a separate, short-lived process (`dvfiles --build-index C:\`, in Windows' background mode: lowest disk
  and memory priority) walks the drive once more and saves every folder (with its file id) and every name to
  `%LOCALAPPDATA%\dvfiles\index` (75 s for 2.1 million items at that priority; the window stays at 10 MB meanwhile,
  since none of the walk's memory is its own): 66 MB for 2.1 million items, names UTF-8 and each only as far as it
  differs from the one before. Before each search the index reads NTFS's change journal, which any user may read
  (`FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`: Windows leaves out the names but keeps each change's folder), and lists
  again only the folders that changed, usually a few dozen in milliseconds. The saved file is mapped, not read, so the
  names stay file pages instead of the process's private memory (8 MB of it while searching, against 72 MB read into
  the heap), and it is let go ten seconds after the last search. It only holds what the user's own walk saw, in the
  user's own profile. When the journal has moved past it (a very busy disk), it is built again in the background.
  A search inside a folder the index never entered (behind a junction, a drive mounted in a folder) and a search
  with a size filter walk instead. Uninstalling removes the index and the settings.
- **Icons** are dvfiles' own: each shape is a distance field, so one sample per pixel gives smooth edges, the bevel's
  light and the soft shadows; an icon is drawn once per size (0.3 ms at 20 px, about 4 ms at 96 px) and kept. The
  toolbar's pictures and line icons are drawn the same way (2-3 ms for all of them on the first frame, at 125%), so
  Windows' icon font (Segoe MDL2 Assets) is no longer loaded.
  Programs' and icon files' own icons are read from the files (`PrivateExtractIconsW`: 0.34 ms a program, where the
  shell's `SHGetFileInfo` took 1.2 ms), shortcuts are read from the .lnk itself (`src/lnk.rs`, the [MS-SHLLINK]
  format: the icon it names, else its target's), so no system image list is kept. The shell is asked only for type
  names and for shortcuts to its own places (Control Panel).
- **Pictures** are decoded by dvfiles (`src/img`): JPEG (baseline and progressive, decoded straight at 1/2, 1/4 or 1/8
  of its size, or from the small picture a camera stores in the file), PNG with its own inflate, GIF's first frame
  and BMP. Each is checked against Windows' own decoder in its tests (62 JPEGs within 3 levels, 299 of 300 PNGs
  exactly, the other a damaged file shown as browsers show it) and was fed hundreds of thousands of damaged copies
  without a crash; a file it cannot read goes to Windows. Store apps' icons (Open with) are PNGs it decodes too.
- **Thumbnails** are made on four threads, for what is on screen and one screen above and below it, and kept in
  dvfiles' own cache (`src/thumbcache.rs`): `%LOCALAPPDATA%\dvfiles\thumbs`, packs of up to 32 MB of which the two
  newest are kept, found by the file's path, size and date, so a changed file is made again. A thumbnail is kept as a
  JPEG made by dvfiles' own encoder (full colour, quality 90: about 3.7 KB for a 120 px photo, where its pixels take
  43 KB; the same size and quality as Windows' encoder makes), one with transparency as its pixels. It holds small
  pictures of the user's files, as Windows' thumbnail cache does, and uninstalling removes it. The newest pack stays
  open while the window makes thumbnails: opening it again and listing the folder for every thumbnail took as long as
  making the thumbnail. The order: dvfiles' cache; for JPEG, PNG, GIF and BMP, dvfiles' decoder (35 ms where Windows
  took 68 ms to make one), with no COM and none of the shell's thumbnail code loaded; for videos, PDFs, RAW and HEIC,
  and a picture dvfiles cannot read, Windows. On a local SSD, asking Windows' cache before decoding was measured and
  dropped: on a folder Windows had already made thumbnails of, decoding was as fast (about 160 ms against 190 ms for
  the window's pictures, the files already in memory), and on new pictures the asking cost 170 ms and 3 MB. On a
  network share or a spinning, removable or optical disk, Windows' cache is still asked first, as reading a whole photo
  there can take far longer than its thumbnail from the cache (reasoned, not measured: there was no such drive to
  test on). A file that is only in the cloud is never read, as that would download it.
- Measured side by side, a Large icons window on 204 new photos has every thumbnail 256-288 ms after it shows
  (Windows alone: 1,197-1,248 ms), and a second window on them 5-26 ms after it shows (Windows' cache: 170-175 ms),
  with 6.7 MB private memory where asking Windows took 10-11 MB. A camera photo whose own small picture is big enough
  is made from the file's first 256 KB, the rest never read. PNG, GIF and BMP are decoded whole before they are
  shrunk, so the four threads share a budget of 128 MB for decoding at once, from each picture's header: 16 4K PNGs
  peaked at 81 MB and the window was back at 9 MB once they were made (a picture that alone would need more goes to
  Windows). A picture is read with a plain read, which never stops another program saving over it.
- **Copying** starts with the first file found instead of a "Calculating..." phase, and copies up to 8 files at a time
  on SSDs (one at a time on spinning disks) with `CopyFile2`, which keeps attributes, times and the "downloaded from the
  internet" mark. A move on one drive is one rename per selected item.
- **Recycling** writes the Recycle Bin's own records: the item is renamed into `$Recycle.Bin` and a small `$I` record
  keeps its path, size and date (layout in `src/recyclefmt.rs`, checked against records the shell wrote). Explorer's
  Recycle Bin lists and restores these items as its own. The shell still does it (and asks before deleting anything
  that cannot be recycled) for OneDrive and other cloud folders, removable and network drives, when "Don't move files
  to the Recycle Bin" is set, and when the item is larger than the bin's limit. On this machine Desktop, Documents and
  Pictures are in OneDrive, so recycling there runs at the shell's speed.
- **Drawing** is GDI into one bitmap per pane, the same text renderer Explorer's file list uses (the set's other
  text apps use DirectWrite because they edit text; file names do not need it). The main window has two small
  bitmaps, its toolbar band and its status band, instead of one the size of the window under the list (3 MB less on
  a large window). Type names are the only thing asked from the shell for a listing, once per extension, on a thread.
- **Dragging out** uses dvfiles' own data object: the `CF_HDROP` list Explorer reads, made in microseconds (5,000
  paths in and out in 4 ms). The shell's data object, used before, needed an item id list for every file, parsed
  from its path: a drag of 1,000 files waited 1 s to start, System32's 4,825 waited 7.6 s. Explorer's own drop
  target takes it (`DVFILES_SHELLTEST=1 cargo test`: a copy with Ctrl, a move on the same drive, each file arriving
  once), and the drag image and "Move to ..." work through the shell's helper, which stores what it needs in it.
  It lets Explorer copy on a thread of its own (`IDataObjectAsyncCapability`), as the shell's object did: dropping
  a 512 MB file on a folder returns in 0.35 s; without it the drop, and the drag in dvfiles, lasted the whole copy
  (2.3-4 s).
  The shell's full right-click menu still needs the item ids; they are now looked up in their folder, parsed once,
  instead of from the desktop down: 0.10 s for 1,000 selected files instead of 1.04 s.
- **No shell where it is not needed**: dropping files is accepted by setting the window style bit `DragAcceptFiles`
  would set, dropped and pasted file lists are read from their DROPFILES block directly, the drag image helper is made
  on the first drag, and shortcuts are resolved from the .lnk file. Measured on a folder of data files: 11.2 MB
  private and 40 modules loaded before, 6.4 MB and 37 modules after (the rest of the difference is the bitmaps).
- **Menus and message boxes** are dvfiles' own windows (`src/ui/menu.rs`, `src/ui/dialog.rs`) with their own
  message loop, as `TrackPopupMenuEx` and `MessageBoxW` have: a menu opens and closes in 10.4 ms where Windows' takes
  19-20 ms (median of 15, opened and closed with Esc). Keeping a closed menu's window hidden for the next one,
  instead of making and destroying one each time, saves about 1-2 ms of that.
- **Show more options** asks the shell for its menu as before (`IContextMenu`: that is how 7-Zip and the like add
  their items) and reads it into dvfiles' menu: each item's text, state, underlined letter and picture, run with
  `InvokeCommand` when picked. Reading takes under a millisecond; the submenus the shell fills only when they open
  (WinRAR's, Open with, Give access to: 8-140 ms each) are filled when they open here too.
- **Opening a file** stays Windows' `ShellExecute`. Reading the associations from the registry, as tried, matched
  Windows' choice for only 301 of the 390 types on this machine that have a program: Windows does not trust a
  program's own claim on a type until the user picks it, and asks instead (.7z, .aac, .3gp here). `ShellExecute` also
  keeps SmartScreen and the downloaded-file warning. What dvfiles replaces is the question: a type with no program
  gets dvfiles' Open with, which starts the choice through Windows (`IAssocHandler::Invoke`).
- **Text** that fits on one line in a script drawn letter by letter (Latin, Greek, Cyrillic) is measured and drawn
  with `ExtTextOutW` (18 us a cell) instead of `DrawTextW` (57 us), which lays out every string in full; a test
  checks the pixels are the same. `DrawTextW` still draws what is shortened with "...", wrapped or shaped. Scrolling
  25 pages of System32 takes 0.19-0.27 s of CPU instead of 0.38-0.53 s.
- **Search refinement**: when the new query can only match a subset of the finished one ("note" to "notepad", an
  added word), the results are filtered in memory: typing on is instant after the first search. The rule is in
  `Query::narrows` and is unit-tested against cases that look narrower but are not.
- **Start**: the window is drawn before it is shown and shown before it is activated (activation starts the input
  method); the address and search boxes and the preview pane are created on first use; fonts are loaded, and the
  first frame's icons drawn, on spare threads; the start folder is read and put in name order on a thread while the
  window is made; the toolbar and the status bar each draw only their own band. From `main` to a visible window on
  System32 (median of 11 starts, `DVFILES_TRACE=1`): 47 ms before these, 37-38 ms now, of which 6-9 ms are making
  the first window (Windows loads its theme and text services then) and 5-7 ms showing it. Windows itself takes about
  16 ms to start the process before `main`, as long as for an exe that does nothing.
- **Idle**: a folder is watched with `ReadDirectoryChangesW` (no polling), and after 8 s without input the bitmaps are
  freed and the working set is handed back to Windows.

## Tried and measured, and left out

- **An own copy loop for small files** (one read, one write, times and attributes set on the handle) was about twice
  as slow as `CopyFile2` for 2,000 small files. Windows recognises `CopyFile2` copies, so the antivirus scan of each
  new file is cheaper; small-file copying is bound by that scan, not by dvfiles.
- **More threads and bigger buffers for search**: 16, 32 or 64 threads and 16 KB to 256 KB buffers all land at
  2.2-2.4 s for 1.2 million items. The cost is opening each of the 123,000 folders through the file system's
  filters. Only reading the volume's master file table would avoid it, and that needs administrator rights.
- **Bigger read buffers for one huge folder**: 64 KB to 4 MB all read 100,000 entries in 108-135 ms (.NET's
  `Directory.GetFiles` takes 221 ms); NTFS needs about 1.1 us per entry, so dvfiles is at that floor.
- **Showing a huge folder in parts while it is read** would save about 0.1 s, and only for folders of tens of
  thousands of files, which already open fully listed in under 0.2 s; it was left out.
- **Reading the master file table** (`FSCTL_ENUM_USN_DATA`, as Everything does) lists a volume without opening its
  folders, but needs administrator rights; the unprivileged change journal plus a saved index gives the same speed
  for searches without them.
- **A resident search index** would hold tens of MB all the time; the saved, mapped index costs nothing between
  searches.
- **POSIX delete** (`FileDispositionInfoEx` with POSIX semantics) deleted 20,000 files no faster than `DeleteFileW`
  (2.3 s either way; recent Windows already deletes that way). Delete workers: 1 takes 5 s, 2 2.6 s, 4 and 8 both
  2.3 s, so 4 are used.
- **Copy workers and preallocation**: 4, 8 and 16 copies at once take the same 0.9 s for 2,000 small files;
  turning `CopyFile2`'s preallocation off (`COPY_FILE_DISABLE_PRE_ALLOCATION`) made it twice as slow.
- **Type names from the registry** (the extension's program id, the user's choice first, then its
  `FriendlyTypeName` through `RegLoadMUIStringW`), to keep the shell out of listings: 176 of this machine's 1,058
  registered extensions came out different from what Explorer shows (".bmp" as "Paintbrush Picture" instead of
  "BMP File", VLC's types, ...), and the shell's type names cost only 0.1 MB, so they stay.
- **IoRing, pure-Rust allocators and text renderers**: IoRing has no directory or metadata operations; the pure-Rust
  allocators target embedded systems; GDI's glyph cache already makes the 40 visible rows cheap. Not tried further.
- **Dates formatted by dvfiles** instead of `GetDateFormatEx` and `GetTimeFormatEx`: those take 3.5 us a row, about
  0.14 ms for a screenful, and own formatting would have to follow every locale's calendars, eras and digits as the
  type names had to follow the shell. Left to Windows.
- **The registry through windows-rs instead of the `winreg` crate**: the set's other apps use `winreg`, and the
  difference is a few functions. Kept, to keep the set alike.
- **Loading COM's core (combase.dll) on first use** like shell32 and ole32: no change in starting time (Windows
  loads it with the first window anyway); left as it was.
- **The text boxes** (address, search, rename) stay Windows' edit controls: an own one would have to redo input
  methods for other languages, selection, undo and screen readers. Their right-click menu is dvfiles'.
- **The segment heap** (`<heapType>SegmentHeap</heapType>` in the manifest) changed memory by less than 0.3 MB in any
  state measured and was left out.
- **Mapping pictures** instead of reading them for thumbnails: no faster (204 photos in 578-596 ms mapped, 560-569 ms
  read, alternating runs), and a mapped file cannot be saved over by another program while it is mapped. Read plainly.
- **Asking Windows' thumbnail cache before decoding**, on a local SSD: see Thumbnails above. On new photos it cost
  170 ms and 3 MB for a window, and where Windows already had the thumbnails, decoding was as fast. Kept only for slow
  drives.
