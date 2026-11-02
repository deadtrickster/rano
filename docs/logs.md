# Log files

A file whose name ends in `.log` opens in log mode by itself. No content sniffing.
`rano -f whatever` forces the same thing for a file named otherwise, and
`rano --edit server.log` opens a log as an ordinary file. The rule applies to the command
line, `M-g` and `F8`.

Log mode opens at the tail, the last screenful, found by scanning back from the end, so a large
log opens without reading the whole file. It then follows: new lines appear as whatever is
writing the file writes them. It opens read-only, because a tailed file belongs to another
process. Typing is refused at the keystroke and says why.

A trailing partial line is held back until its newline arrives. Nothing past where the file had
reached when you opened it is coloured.

Line numbers are the file's, counted in the background while you look at the buffer. Until
that count answers, the gutter is blank and the status line says `Ln ?`, because a tail's row
1 is not the file's line 1.

A view scrolled away from the tail stays put while the file grows.
