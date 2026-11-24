(() => {
  const key = '__rustsearchPick';
  window[key]?.dispose();
  const outline = document.createElement('div');
  outline.style.cssText = 'position:fixed;pointer-events:none;z-index:2147483647;box-shadow:inset 0 0 0 3px #00aa99;display:none';
  document.documentElement.append(outline);
  const pick = { selector: null, dispose };
  window[key] = pick;
  function eligible(element) { return element instanceof Element && element !== document.body && element !== document.documentElement; }
  function paint(event) {
    if (!eligible(event.target)) { outline.style.display = 'none'; return; }
    const rect = event.target.getBoundingClientRect();
    Object.assign(outline.style, {display:'block', left:`${rect.left}px`, top:`${rect.top}px`, width:`${rect.width}px`, height:`${rect.height}px`});
  }
  function choose(event) {
    event.preventDefault();
    event.stopImmediatePropagation();
    if (!eligible(event.target)) return;
    const chain = [];
    for (let el = event.target; el && el !== document.documentElement; el = el.parentElement) {
      if (el.id) {
        const id = '#' + CSS.escape(el.id);
        if (document.querySelectorAll(id).length === 1) { chain.unshift(id); break; }
      }
      const peers = [...el.parentElement.children].filter(peer => peer.localName === el.localName);
      chain.unshift(`${CSS.escape(el.localName)}:nth-of-type(${peers.indexOf(el) + 1})`);
    }
    pick.selector = chain.join(' > ');
    paint(event);
    window.removeEventListener('pointermove', paint, true);
    window.removeEventListener('click', choose, true);
  }
  function dispose() {
    outline.remove();
    window.removeEventListener('pointermove', paint, true);
    window.removeEventListener('click', choose, true);
    delete window[key];
  }
  window.addEventListener('pointermove', paint, true);
  window.addEventListener('click', choose, true);
})();
