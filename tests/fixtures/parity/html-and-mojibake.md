# Raw HTML and repair

<div class="note">
literal &amp; text inside a block
</div>

Inline <span style="color: red">HTML</span> and <br> stay visible, as does an
<!-- HTML comment --> in the middle of a sentence.

<script>alert("never runs")</script>

<details>
<summary>Summary</summary>

Markdown *inside* an HTML block.

</details>

Mojibake is repaired before rendering: itâ€™s a â€œquotedâ€ phrase â€” with a dash
â€“ and an ellipsisâ€¦ Correct text is left alone: it’s “quoted” — fine.
