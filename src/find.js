((text, backwards) => {
    const selection = getSelection();
    const previous = window.__rustsearchFind;
    if (!text) {
        if (previous && selection.rangeCount === 1 &&
            selection.anchorNode === previous.range.startContainer &&
            selection.anchorOffset === previous.range.startOffset) {
            selection.removeAllRanges();
        }
        delete window.__rustsearchFind;
        return;
    }
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const needle = new RegExp(RegExp.escape(text), "giu");
    const matches = [];
    for (let node; (node = walker.nextNode());) {
        for (const match of node.data.matchAll(needle)) {
            const range = document.createRange();
            range.setStart(node, match.index);
            range.setEnd(node, match.index + match[0].length);
            if (range.getClientRects().length &&
                getComputedStyle(node.parentElement).visibility === "visible") {
                matches.push(range);
            }
        }
    }
    if (!matches.length) return;
    const index = previous?.text === text
        ? matches.findIndex(range => range.compareBoundaryPoints(Range.START_TO_START, previous.range) === 0)
        : -1;
    const offset = backwards ? (index < 0 ? -1 : index - 1) : index + 1;
    const range = matches[(offset + matches.length) % matches.length];
    selection.removeAllRanges();
    selection.addRange(range);
    range.startContainer.parentElement.scrollIntoView({block: "center"});
    window.__rustsearchFind = {text, range};
})
