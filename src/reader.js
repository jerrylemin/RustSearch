(() => {
  const key = '__rustsearchReader';
  if (window[key]) {
    window[key].remove();
    delete window[key];
    return;
  }
  const source = document.querySelector('article, main, [role="main"]') || document.body;
  if (!source) return;
  const view = document.createElement('div');
  view.setAttribute('role', 'document');
  view.style.cssText = 'position:fixed;inset:0;z-index:2147483647;overflow:auto;background:#f6f3ed;color:#252422;color-scheme:light';
  const article = document.createElement('article');
  article.style.cssText = 'max-width:70ch;margin:0 auto;padding:36px 24px;font:19px/1.65 Georgia,serif;overflow-wrap:anywhere';
  for (const node of source.querySelectorAll('h1,h2,h3,h4,p,blockquote,li,pre')) {
    if (node.closest('nav,aside,footer,form,[hidden]')) continue;
    if (node.parentElement?.closest('li,blockquote')) continue;
    const text = node.innerText?.trim();
    if (!text) continue;
    const section = document.createElement(node.localName === 'li' ? 'p' : node.localName);
    section.textContent = node.localName === 'li' ? '• ' + text : text;
    if (node.localName === 'pre') section.style.whiteSpace = 'pre-wrap';
    article.append(section);
  }
  if (!article.children.length) return;
  const shadow = view.attachShadow({mode:'closed'});
  shadow.append(article);
  document.documentElement.append(view);
  window[key] = view;
})();
