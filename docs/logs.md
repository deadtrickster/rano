# Log files

A file whose name ends in `.log` opens in log mode by itself. The name is the whole rule,
with no `/var/log/` case and no content sniffing. `rano -f whatever` forces the same thing for
a file named otherwise, and `rano --edit server.log` opens a log as an ordinary file. The rule
is applied at every door a file comes in by: the command line, a second file on it, `M-g`,
`F8`.

Log mode opens at the tail, the last screenful, found by scanning backwards from the end, so a
large log opens for a read of a few hundred KiB rather than the whole file. It then follows:
new lines appear as whatever is writing the file writes them. It opens read-only, because a
tailed file belongs to another process. Typing is refused at the keystroke and says why.

A row appears only once the file has finished writing it. A trailing partial line is held back
until its newline arrives. Nothing past where the file had reached when you opened it is
coloured.

Line numbers are the file's, counted in the background while you look at the buffer. Until
that count answers, the gutter is blank and the status line says `Ln ?`, because a tail's row
1 is not the file's line 1.

A view you have scrolled away from stays where you put it. Following a file does not move your
screen.
