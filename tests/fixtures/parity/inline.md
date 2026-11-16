# Inline formatting

Plain text with *emphasis*, **strong**, ***both***, ~~strikethrough~~ and
`inline code`. A hard break follows this line\
and ends here. Two trailing spaces also break  
the line.

Entities: &amp; &lt; &gt; &quot; &copy; &#169; &#x1F600; &nbsp;wide&nbsp;gap and a
bare & ampersand, a < less-than and a > greater-than.

Escapes: \*not emphasis\* \`not code\` \# not a heading \[not a link\].

Code spans keep `<tags> & "quotes"` and `` a ` backtick `` and `two
lines` joined by a space.

Links: [inline](guide.md), [with title](guide.md "A title"), [reference][ref],
[collapsed][], <https://example.com/auto>, <mailto:someone@example.com> and a
[link with *emphasis* and `code`](other.md#anchor).

Images contribute no text: ![alternative text](chart.png) sits between words,
and a linked image [![badge](badge.svg)](https://example.com/) does too.

[ref]: reference.md
[collapsed]: collapsed.md
