/* OpenCW web shell.
 *
 * Vanilla ES2020, single file, zero external requests: every asset is served
 * from /static/ on the same origin and every read/write goes through the
 * OpenCW HTTP API (which itself never bypasses the kernels).
 *
 * Sections mirror the CLI surfaces: dashboard, trial balance, documents,
 * accounts, entries, signoff, operations (preview -> commit), manifest.
 */
(function () {
  'use strict';

  /* ------------------------------------------------------------ utilities */

  function mk(tag, props, children) {
    var n = document.createElement(tag);
    if (props) {
      Object.keys(props).forEach(function (k) {
        var v = props[k];
        if (v === null || v === undefined || v === false) return;
        if (k === 'class') n.className = v;
        else if (k === 'text') n.textContent = String(v);
        else if (k === 'value') n.value = v;
        else if (k === 'dataset') Object.assign(n.dataset, v);
        else if (k === 'style' && typeof v === 'object') Object.assign(n.style, v);
        else if (k.length > 2 && k.slice(0, 2) === 'on' && typeof v === 'function') {
          n.addEventListener(k.slice(2), v);
        } else if (v === true) n.setAttribute(k, '');
        else n.setAttribute(k, String(v));
      });
    }
    if (children !== null && children !== undefined) {
      (Array.isArray(children) ? children : [children]).forEach(function (c) {
        if (c === null || c === undefined || c === false) return;
        n.appendChild(c instanceof Node ? c : document.createTextNode(String(c)));
      });
    }
    return n;
  }

  var fmt = {
    num: function (v) {
      if (v === null || v === undefined || v === '') return '';
      var n = Number(v);
      if (!isFinite(n)) return String(v);
      return n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
    },
    int: function (v) {
      if (v === null || v === undefined || v === '') return '';
      var n = Number(v);
      if (!isFinite(n)) return String(v);
      return n.toLocaleString('en-US');
    },
    bytes: function (v) {
      if (v === null || v === undefined || v === '') return '';
      var n = Number(v);
      if (!isFinite(n)) return String(v);
      if (n < 1024) return n + ' B';
      if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
      if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + ' MB';
      return (n / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    },
    sha: function (s) {
      if (!s) return '';
      return String(s).slice(0, 12);
    },
    amount: function (raw) {
      var s = String(raw === null || raw === undefined ? '' : raw).replace(/[,\s$]/g, '');
      if (!s) return NaN;
      var neg = /^\(.*\)$/.test(s);
      if (neg) s = s.slice(1, -1);
      var n = Number(s);
      if (!isFinite(n)) return NaN;
      return neg ? -n : n;
    }
  };

  function text(v) {
    return v === null || v === undefined ? '' : String(v);
  }

  /* ------------------------------------------------------------- transport */

  function ApiError(code, message, status) {
    this.name = 'ApiError';
    this.code = code || 'error';
    this.message = message || 'Request failed';
    this.status = status || 0;
  }
  ApiError.prototype = Object.create(Error.prototype);

  async function request(method, url, body) {
    var opts = { method: method, cache: 'no-store', headers: { Accept: 'application/json' } };
    if (body !== undefined && body !== null) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    var res;
    try {
      res = await fetch(url, opts);
    } catch (e) {
      throw new ApiError('offline', 'Cannot reach the OpenCW server (' + e.message + ')', 0);
    }
    var raw = '';
    try { raw = await res.text(); } catch (e) { raw = ''; }
    var data = null;
    if (raw) {
      try { data = JSON.parse(raw); } catch (e) { data = null; }
    }
    if (!res.ok || (data && data.error)) {
      var e2 = (data && data.error) || {};
      var err = new ApiError(
        e2.code || ('http_' + res.status),
        e2.message || (data && data.message) || ('HTTP ' + res.status + ' ' + (res.statusText || '')),
        res.status
      );
      /* A refused mutation still carries the full computed preview (e.g. a
         blocked renumber cascade names every blocking issue and the plan it
         refused). Keep the body on the error so callers can render it. */
      err.body = data;
      throw err;
    }
    if (data === null) {
      throw new ApiError('bad_response', 'Server returned a response that is not JSON', res.status);
    }
    return data;
  }

  function getJSON(url) { return request('GET', url); }
  function postJSON(url, body) { return request('POST', url, body || {}); }

  /* ---------------------------------------------------------------- toasts */

  var toastHost = null;

  function toastHostNode() {
    if (!toastHost) {
      toastHost = document.getElementById('toasts');
      if (!toastHost) {
        toastHost = mk('div', { class: 'toasts' });
        document.body.appendChild(toastHost);
      }
    }
    return toastHost;
  }

  function toast(message, kind, code) {
    var t = mk('div', { class: 'toast ' + (kind || '') }, [
      mk('div', { text: message }),
      code ? mk('span', { class: 'code', text: code }) : null
    ]);
    var remove = function () { if (t.parentNode) t.parentNode.removeChild(t); };
    t.addEventListener('click', remove);
    toastHostNode().appendChild(t);
    setTimeout(remove, kind === 'err' ? 9000 : 4200);
  }

  function toastErr(e) {
    toast(e && e.message ? e.message : String(e), 'err', e && e.code ? e.code : '');
  }

  function errorPane(e, retry) {
    return mk('div', { class: 'error-pane' }, [
      mk('div', { text: (e && e.message) || String(e) }),
      mk('span', { class: 'code', text: (e && e.code) || '' }),
      retry ? mk('div', { class: 'btn-row', style: { marginTop: '8px' } },
        mk('button', { class: 'btn', text: 'Retry', onclick: retry })) : null
    ]);
  }

  /* ----------------------------------------------------------------- state */

  var S = {
    overview: null,
    overviewError: null,
    navCollapsed: {},
    view: 'dashboard',
    tb: {
      year: 0, period: 12, group: 'A', entity: '', limit: 0, offset: 0,
      rows: [], totals: null, columns: [], total: null, priorYear: null,
      sort: { key: 'acct', dir: 1 }
    },
    docs: {
      lens: 'dms',            /* 'dms' = Document Manager | 'register' = SH table */
      q: '', klass: 'all', internals: false,
      dirFilter: null, selected: null, collapsed: {},
      data: null, regCache: null
    },
    accounts: { q: '', rows: [], selected: '', detail: null },
    entries: { acct: '', list: [], selected: '', count: null },
    signoff: { doc: '', data: null },
    ops: { audit: [] },
    manifest: { path: '', files: [], total: null, a: '', b: '', diff: null }
  };

  var mount = null;
  var mainPane = null;

  /* --------------------------------------------------------------- shell UI */

  var NAV = [
    {
      group: 'Engagement',
      items: [
        { id: 'dashboard', label: 'Dashboard', glyph: '\u25A6' },
        { id: 'tb', label: 'Trial Balance', glyph: '\u229E' },
        { id: 'docs', label: 'Documents', glyph: '\u2261' }
      ]
    },
    {
      group: 'Data',
      items: [
        { id: 'accounts', label: 'Accounts', glyph: '\u25A4' },
        { id: 'entries', label: 'Entries', glyph: '\u0192' },
        { id: 'signoff', label: 'Signoff', glyph: '\u2713' }
      ]
    },
    {
      group: 'Administration',
      items: [
        { id: 'operations', label: 'Operations bay', glyph: '\u2699' },
        { id: 'manifest', label: 'Manifest', glyph: '\u2687' }
      ]
    }
  ];

  var VIEW_TITLES = {
    dashboard: 'Dashboard',
    tb: 'Trial Balance',
    docs: 'Document Manager',
    accounts: 'Account Properties',
    entries: 'Adjusting Entries',
    signoff: 'Signoff',
    operations: 'Operations Bay',
    manifest: 'Manifest'
  };

  function buildShell() {
    mount = document.getElementById('app');
    if (!mount) {
      mount = mk('div', { id: 'app' });
      document.body.insertBefore(mount, document.body.firstChild);
    }
    mount.replaceChildren();

    var engName = mk('div', { class: 'eng-name', text: 'Loading engagement\u2026' });
    var engPath = mk('div', { class: 'eng-path', text: '' });
    var health = mk('div', { class: 'health' }, [
      mk('span', { class: 'dot' }),
      mk('span', { class: 'hlabel', text: 'checking' })
    ]);

    var top = mk('header', { class: 'topbar' }, [
      mk('div', { class: 'brand' }, ['OpenCW', mk('small', { text: 'engagement workspace' })]),
      mk('div', { class: 'eng' }, [engName, engPath]),
      health,
      mk('div', { class: 'topbar-actions' }, [
        mk('button', {
          class: 'btn', text: 'Refresh', title: 'Reload engagement state',
          onclick: function () { refreshOverview(true).then(reloadCurrent, reloadCurrent); }
        }),
        mk('button', { class: 'btn', text: 'Print', title: 'Print the current view', onclick: function () { window.print(); } })
      ])
    ]);
    S.el = { engName: engName, engPath: engPath, health: health };

    var nav = mk('nav', { class: 'sidenav', 'aria-label': 'Sections' });
    NAV.forEach(function (g) {
      var collapsed = !!S.navCollapsed[g.group];
      var caret = mk('span', { class: 'tree-toggle', text: collapsed ? '\u25B8' : '\u25BE' });
      var head = mk('div', {
        class: 'nav-group-label',
        style: { display: 'flex', gap: '6px', alignItems: 'center', cursor: 'pointer', userSelect: 'none' },
        onclick: function () { S.navCollapsed[g.group] = !S.navCollapsed[g.group]; buildShell(); renderRoute(); }
      }, [caret, mk('span', { text: g.group })]);
      nav.appendChild(head);
      if (collapsed) return;
      g.items.forEach(function (it) {
        nav.appendChild(mk('a', {
          class: 'nav-item' + (S.view === it.id ? ' active' : ''),
          href: '#/' + it.id,
          dataset: { view: it.id }
        }, [
          mk('span', { class: 'glyph', text: it.glyph }),
          mk('span', { text: it.label })
        ]));
      });
    });
    nav.appendChild(mk('div', { class: 'nav-foot' }, [
      mk('div', { text: 'Offline UI \u00b7 no external assets' }),
      mk('span', { class: 'mono', text: 'read \u2192 preview \u2192 commit' })
    ]));

    mainPane = mk('main', { id: 'main-pane', tabindex: '-1' });
    mount.appendChild(top);
    mount.appendChild(nav);
    mount.appendChild(mainPane);
    /* The shell can be rebuilt (nav groups collapse), which throws away the
       header nodes; repaint them from the state already in hand. */
    setHeader();
  }

  function setHealth(ok, label) {
    if (!S.el) return;
    var h = S.el.health;
    var dot = h.querySelector('.dot');
    var lab = h.querySelector('.hlabel');
    dot.className = 'dot ' + (ok ? 'ok' : 'bad');
    lab.textContent = label || (ok ? 'healthy' : 'attention');
    h.title = ok ? 'Engagement tables decoded and consistent' : 'One or more engagement tables need attention';
  }

  function setHeader() {
    if (!S.el) return;
    var o = S.overview;
    if (!o) {
      if (S.overviewError) {
        S.el.engName.textContent = 'Engagement unavailable';
        S.el.engPath.textContent = '';
        setHealth(false, 'unreachable');
      }
      return;
    }
    var stem = overviewStem(o);
    var path = overviewPath(o);
    S.el.engName.textContent = stem ? String(stem) : '(unnamed engagement)';
    S.el.engPath.textContent = path ? String(path) : '';
    S.el.engName.title = String(stem || '');
    S.el.engPath.title = String(path || '');
    var healthy = o.healthy;
    if (healthy === undefined || healthy === null) healthy = !S.overviewError;
    setHealth(!!healthy, healthy ? 'healthy' : 'attention');
  }

  async function refreshOverview(force) {
    if (S.overview && !force) { setHeader(); return S.overview; }
    try {
      S.overview = await getJSON('/api/overview');
      S.overviewError = null;
      var ovPath = overviewPath(S.overview);
      if (ovPath && !S.manifest.path) S.manifest.path = ovPath;
    } catch (e) {
      S.overviewError = e;
      if (force) toastErr(e);
    }
    setHeader();
    return S.overview;
  }

  /* ---------------------------------------------------------------- router */

  var VIEWS = {};

  function currentRoute() {
    var h = (location.hash || '').replace(/^#\/?/, '');
    var id = h.split('/')[0] || 'dashboard';
    return VIEWS[id] ? id : 'dashboard';
  }

  function navigate(id) {
    if (location.hash !== '#/' + id) {
      location.hash = '#/' + id;
      return;
    }
    renderRoute();
  }

  function renderRoute() {
    var id = currentRoute();
    S.view = id;
    if (!mainPane) return;
    Array.prototype.forEach.call(document.querySelectorAll('.nav-item'), function (a) {
      a.classList.toggle('active', a.dataset.view === id);
    });
    /* Each route render owns a fresh pane: a slow view that resolves after the
       user navigated away appends into a detached node and cannot clobber the
       new view. */
    var pane = mk('div', { class: 'pane' });
    mainPane.replaceChildren(pane);
    mainPane.scrollTop = 0;
    /* A view that throws or rejects must surface a pane, never an eternal
       spinner; a stale pane (the user already navigated) is left alone. */
    try {
      var r = VIEWS[id](pane);
      if (r && typeof r.catch === 'function') {
        r.catch(function (e) { viewFailed(pane, e); });
      }
    } catch (e) {
      viewFailed(pane, e);
    }
  }

  function viewFailed(pane, e) {
    if (!pane.isConnected) return;
    pane.replaceChildren(errorPane(e, function () { renderRoute(); }));
    toastErr(e);
  }

  /* Refresh means "the engagement changed on disk": drop every cached decode so
     the next paint re-reads through the API instead of replaying stale rows. */
  function invalidateCaches() {
    S.docs.data = null;
    S.docs.regCache = null;
    S.accounts.rows = [];
    S.accounts.detail = null;
    S.entries.list = [];
    S.signoff.data = null;
    S.tb.rows = [];
    S.tb.totals = null;
    S.tb.total = null;
  }

  function reloadCurrent() {
    invalidateCaches();
    renderRoute();
  }

  function viewHead(title, subtitle, actions) {
    return mk('div', { class: 'view-head' }, [
      mk('div', null, [
        mk('h1', { class: 'view-title', text: title }),
        subtitle ? mk('div', { class: 'view-sub', text: subtitle }) : null
      ]),
      mk('div', { class: 'spacer' }),
      actions ? mk('div', { class: 'btn-row' }, actions) : null
    ]);
  }

  /* ------------------------------------------------------------- dashboard */

  function censusOf(o) {
    if (!o) return {};
    return o.census || o.counts || {};
  }

  function overviewStem(o) {
    if (!o) return '';
    /* The server sends both `stem` (the engagement name) and `engagement`
       (the directory); `stem` wins, and a string `engagement` is only a
       fallback for older/alternate payloads. */
    if (o.stem) return String(o.stem);
    var eng = o.engagement;
    if (typeof eng === 'string') return eng;
    if (eng && typeof eng === 'object') return String(eng.stem || eng.name || '');
    return '';
  }

  function overviewPath(o) {
    if (!o) return '';
    if (o.path) return String(o.path);
    var eng = o.engagement;
    if (eng && typeof eng === 'object' && eng.path) return String(eng.path);
    return '';
  }

  function numCensus(c, keys) {
    for (var i = 0; i < keys.length; i++) {
      if (c[keys[i]] !== undefined && c[keys[i]] !== null) return Number(c[keys[i]]);
    }
    return null;
  }

  VIEWS.dashboard = async function (root) {
    root.replaceChildren();
    var o = await refreshOverview(true);
    root.appendChild(viewHead('Dashboard', o ? overviewStem(o) : 'Engagement state', [
      mk('button', { class: 'btn', text: 'Open trial balance', onclick: function () { navigate('tb'); } }),
      mk('button', { class: 'btn', text: 'Operations bay', onclick: function () { navigate('operations'); } })
    ]));
    if (!o) {
      root.appendChild(errorPane(S.overviewError || new ApiError('overview', 'Overview unavailable'), function () { VIEWS.dashboard(root); }));
      return;
    }
    var c = censusOf(o);
    var cards = [
      { k: 'Posting accounts', v: numCensus(c, ['am_posting', 'amPosting', 'posting']), hint: 'AM rows flagged POST' },
      { k: 'AM chart rows', v: numCensus(c, ['am_rows', 'amRows']), hint: 'accounts table' },
      { k: 'Grouping schemes', v: numCensus(c, ['mp_schemes', 'mpSchemes']), hint: 'MP mappings' },
      { k: 'Live documents', v: numCensus(c, ['sh_live', 'shLive']), hint: 'SH rows in tree' },
      { k: 'Deleted documents', v: numCensus(c, ['sh_deleted', 'shDeleted']), hint: 'tombstones' },
      { k: 'CaseView forms', v: numCensus(c, ['sh_forms', 'shForms']), hint: 'response documents' },
      { k: 'Balance rows', v: numCensus(c, ['bl_rows', 'blRows']), hint: 'BL cube records' }
    ];
    var grid = mk('div', { class: 'cards' });
    var healthy = o.healthy === undefined || o.healthy === null ? true : !!o.healthy;
    grid.appendChild(mk('div', { class: 'card ' + (healthy ? '' : 'bad') }, [
      mk('div', { class: 'k', text: 'Engagement health' }),
      mk('div', { class: 'v', text: healthy ? 'Healthy' : 'Attention' }),
      mk('div', { class: 'hint' }, mk('span', { class: 'pill ' + (healthy ? 'pill-ok' : 'pill-bad'), text: healthy ? 'decoded' : 'check tables' }))
    ]));
    cards.forEach(function (c2) {
      grid.appendChild(mk('div', { class: 'card' }, [
        mk('div', { class: 'k', text: c2.k }),
        mk('div', { class: 'v', text: c2.v === null ? '\u2014' : fmt.int(c2.v) }),
        mk('div', { class: 'hint', text: c2.hint })
      ]));
    });
    root.appendChild(grid);

    root.appendChild(mk('h2', { class: 'section', text: 'Engagement' }));
    var kv = mk('table', { class: 'kv' }, mk('tbody', null, [
      mk('tr', null, [mk('th', { text: 'Stem' }), mk('td', { class: 'mono', text: overviewStem(o) })]),
      mk('tr', null, [mk('th', { text: 'Path' }), mk('td', { class: 'mono', text: overviewPath(o) })]),
      mk('tr', null, [mk('th', { text: 'Health' }), mk('td', null,
        mk('span', { class: 'pill ' + (healthy ? 'pill-ok' : 'pill-warn'), text: healthy ? 'OK' : 'needs attention' }))]),
      mk('tr', null, [mk('th', { text: 'Work area' }), mk('td', { class: 'mono', text: workDirHint(overviewPath(o)) })])
    ]));
    root.appendChild(kv);

    root.appendChild(mk('h2', { class: 'section', text: 'Jump to' }));
    root.appendChild(mk('div', { class: 'btn-row' }, [
      mk('button', { class: 'btn', text: 'Trial balance grid', onclick: function () { navigate('tb'); } }),
      mk('button', { class: 'btn', text: 'Documents tree', onclick: function () { navigate('docs'); } }),
      mk('button', { class: 'btn', text: 'Adjusting entries', onclick: function () { navigate('entries'); } }),
      mk('button', { class: 'btn', text: 'Signoff status', onclick: function () { navigate('signoff'); } }),
      mk('button', { class: 'btn', text: 'Manifest viewer', onclick: function () { navigate('manifest'); } })
    ]));
  };

  function workDirHint(p) {
    if (!p) return '(unknown)';
    var norm = String(p).replace(/[\\/]+$/, '');
    var sep = norm.indexOf('\\') >= 0 && norm.indexOf('/') < 0 ? '\\' : '/';
    var idx = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
    var parent = idx > 0 ? norm.slice(0, idx) : norm;
    return parent + sep + 'work' + sep;
  }

  /* --------------------------------------------------------------- TB grid */

  var TB_KEYS = ['opening', 'period', 'ytd_movement', 'ytd', 'prior_opening', 'prior_period', 'prior_ytd', 'variance_period', 'variance_ytd'];
  var TB_LABELS = {
    opening: 'Opening',
    period: 'Period movement',
    ytd_movement: 'YTD movement',
    ytd: 'YTD balance',
    prior_opening: 'Prior opening',
    prior_period: 'Prior movement',
    prior_ytd: 'Prior YTD',
    variance_period: 'Movement variance',
    variance_ytd: 'YTD variance'
  };

  function tbQuery() {
    var t = S.tb;
    var q = [];
    q.push('year=' + encodeURIComponent(t.year));
    q.push('period=' + encodeURIComponent(t.period));
    q.push('group=' + encodeURIComponent(t.group));
    if (t.entity) q.push('entity=' + encodeURIComponent(t.entity));
    if (t.limit) q.push('limit=' + encodeURIComponent(t.limit));
    if (t.offset) q.push('offset=' + encodeURIComponent(t.offset));
    return '/api/tb?' + q.join('&');
  }

  async function loadTb() {
    var data = await getJSON(tbQuery());
    var t = S.tb;
    t.rows = data.rows || [];
    t.totals = data.totals || null;
    t.columns = data.columns || [];
    t.priorYear = data.prior_year === undefined ? null : data.prior_year;
    t.total = data.total === undefined || data.total === null ? t.rows.length : Number(data.total);
    t.error = null;
    return t;
  }

  VIEWS.tb = async function (root) {
    root.replaceChildren();
    root.appendChild(viewHead('Trial Balance', 'BL balance cube \u2192 period / YTD columns with prior-year comparison', [
      mk('button', { class: 'btn', text: 'Download CSV', onclick: function () { downloadTbCsv(); } })
    ]));

    var year = mk('input', { type: 'number', min: '0', max: '9', value: String(S.tb.year), 'aria-label': 'year' });
    var period = mk('input', { type: 'number', min: '1', max: '12', value: String(S.tb.period), 'aria-label': 'period' });
    var group = mk('select', { 'aria-label': 'group' }, [
      mk('option', { value: 'A', text: 'Actual basis (A)' }),
      mk('option', { value: 'T', text: 'Tax basis (T)' })
    ]);
    group.value = S.tb.group;
    var entity = mk('input', { type: 'text', value: S.tb.entity, placeholder: 'all entities', size: '10', 'aria-label': 'entity' });
    var limit = mk('select', { 'aria-label': 'row limit' }, [
      mk('option', { value: '0', text: 'All rows' }),
      mk('option', { value: '100', text: '100' }),
      mk('option', { value: '250', text: '250' }),
      mk('option', { value: '500', text: '500' })
    ]);
    limit.value = String(S.tb.limit);
    var apply = mk('button', { class: 'btn btn-primary', text: 'Apply' });
    apply.addEventListener('click', function () {
      S.tb.year = Number(year.value) || 0;
      S.tb.period = Math.min(12, Math.max(1, Number(period.value) || 12));
      S.tb.group = group.value;
      S.tb.entity = entity.value.trim();
      S.tb.limit = Number(limit.value) || 0;
      S.tb.offset = 0;
      VIEWS.tb(root);
    });
    [year, period, entity].forEach(function (inp) {
      inp.addEventListener('keydown', function (e) { if (e.key === 'Enter') apply.click(); });
    });
    group.addEventListener('change', function () { apply.click(); });
    limit.addEventListener('change', function () { apply.click(); });

    root.appendChild(mk('div', { class: 'toolbar' }, [
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Year' }), year]),
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Period' }), period]),
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Basis' }), group]),
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Entity' }), entity]),
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Rows' }), limit]),
      apply,
      mk('span', { class: 'sep' }),
      mk('span', { class: 'small muted', text: 'Click a column header to sort \u00b7 click a row for account properties' })
    ]));

    var host = mk('div');
    root.appendChild(host);

    try {
      await loadTb();
    } catch (e) {
      host.appendChild(errorPane(e, function () { VIEWS.tb(root); }));
      toastErr(e);
      return;
    }

    var t = S.tb;
    if (!t.rows.length) {
      host.appendChild(mk('div', { class: 'empty', text: 'No trial-balance rows for this year / period / basis. Try a lower period or a different basis.' }));
      return;
    }

    var colLabels = t.columns.length ? t.columns : TB_KEYS.slice(0, 9).map(function (k) { return TB_LABELS[k]; });
    var keys = TB_KEYS.slice(0, colLabels.length);

    var sorted = t.rows.slice();
    var sk = t.sort.key;
    var dir = t.sort.dir;
    sorted.sort(function (a, b) {
      var av = a[sk], bv = b[sk];
      if (sk === 'acct' || sk === 'desc') {
        return String(av || '').localeCompare(String(bv || '')) * dir;
      }
      av = Number(av) || 0; bv = Number(bv) || 0;
      if (av === bv) return String(a.acct).localeCompare(String(b.acct)) * dir;
      return (av - bv) * dir;
    });

    var thead = mk('thead');
    var hrow = mk('tr');
    function sortableTh(key, label, numeric, cls) {
      var mark = sk === key ? (dir > 0 ? ' \u25B2' : ' \u25BC') : '';
      var th = mk('th', {
        class: (numeric ? 'num ' : '') + (cls || ''),
        onclick: function () {
          if (sk === key) S.tb.sort.dir = -dir; else { S.tb.sort.key = key; S.tb.sort.dir = numeric ? -1 : 1; }
          VIEWS.tb(root);
        },
        title: 'Sort by ' + label
      }, [label, mk('span', { class: 'sortmark', text: mark })]);
      return th;
    }
    hrow.appendChild(sortableTh('acct', 'Account', false, 'mono'));
    hrow.appendChild(sortableTh('desc', 'Description', false));
    keys.forEach(function (k, i) { hrow.appendChild(sortableTh(k, colLabels[i], true)); });
    thead.appendChild(hrow);

    var tbody = mk('tbody');
    sorted.forEach(function (r) {
      var tr = mk('tr', {
        class: 'rowlink',
        title: 'Open ' + r.acct + ' properties',
        onclick: function () {
          S.accounts.selected = r.acct;
          S.accounts.detail = null;
          navigate('accounts');
        }
      });
      tr.appendChild(mk('td', { class: 'acct', text: text(r.acct) }));
      tr.appendChild(mk('td', { class: 'desc', text: text(r.desc), title: text(r.desc) }));
      keys.forEach(function (k) {
        var v = Number(r[k]) || 0;
        var cls = 'num';
        if (k.indexOf('variance') === 0) {
          /* Highlight only material movements: a variance is hot when it moves
             the prior balance by >=5%, or by >=10,000 when there is no prior
             balance to compare against. Direction is coloured independently. */
          var base = Math.abs(Number(k === 'variance_ytd' ? r.prior_ytd : r.prior_period) || 0);
          var hot = base > 0 ? Math.abs(v) / base >= 0.05 : Math.abs(v) >= 10000;
          if (hot) cls += ' v-hot';
          if (v < 0) cls += ' v-neg'; else if (v > 0) cls += ' v-pos';
        }
        tr.appendChild(mk('td', { class: cls, text: v === 0 ? '\u2013' : fmt.num(v) }));
      });
      tbody.appendChild(tr);
    });

    var tfoot = mk('tfoot');
    if (t.totals) {
      var fr = mk('tr');
      fr.appendChild(mk('td', { text: 'Totals' }));
      fr.appendChild(mk('td', { text: fmt.int(sorted.length) + ' rows' }));
      keys.forEach(function (k) {
        fr.appendChild(mk('td', { class: 'num', text: fmt.num(t.totals[k]) }));
      });
      tfoot.appendChild(fr);
    }

    var table = mk('table', { class: 'grid' }, [thead, tbody, tfoot]);
    var wrap = mk('div', { class: 'grid-wrap' }, table);
    host.appendChild(wrap);

    var meta = [];
    meta.push(sorted.length + ' of ' + fmt.int(t.total) + ' rows');
    if (t.priorYear !== null && t.priorYear !== undefined) meta.push('comparing Y' + t.priorYear);
    meta.push('basis ' + t.group);
    var footRow = mk('div', { class: 'btn-row', style: { marginTop: '6px' } }, [
      mk('span', { class: 'small muted', text: meta.join(' \u00b7 ') })
    ]);
    /* `offset` is only meaningful with a row limit; when the server reports more
       rows than were sent, page through them instead of silently truncating. */
    var pageSize = Number(t.limit) || 0;
    if (pageSize > 0 && t.total > pageSize) {
      var start = Number(t.offset) || 0;
      var prev = mk('button', { class: 'btn btn-mini', text: '\u2039 Previous', disabled: start <= 0 });
      var next = mk('button', { class: 'btn btn-mini', text: 'Next \u203a', disabled: start + pageSize >= t.total });
      prev.addEventListener('click', function () {
        t.offset = Math.max(0, start - pageSize);
        VIEWS.tb(root);
      });
      next.addEventListener('click', function () {
        t.offset = start + pageSize;
        VIEWS.tb(root);
      });
      footRow.appendChild(mk('span', { style: { flex: '1' } }));
      footRow.appendChild(mk('span', { class: 'small muted', text: 'rows ' + fmt.int(start + 1) + '\u2013' + fmt.int(start + sorted.length) }));
      footRow.appendChild(prev);
      footRow.appendChild(next);
    }
    host.appendChild(footRow);
  };

  function csvCell(v) {
    var s = v === null || v === undefined ? '' : String(v);
    if (/[",\n\r]/.test(s)) return '"' + s.replace(/"/g, '""') + '"';
    return s;
  }

  function downloadTbCsv() {
    var t = S.tb;
    if (!t.rows.length) { toast('Nothing to export \u2014 load the grid first', 'err', 'empty'); return; }
    var labels = t.columns.length ? t.columns : TB_KEYS.slice(0, 9).map(function (k) { return TB_LABELS[k]; });
    var keys = TB_KEYS.slice(0, labels.length);
    var lines = [];
    lines.push(['Account', 'Description'].concat(labels).map(csvCell).join(','));
    var sorted = t.rows.slice().sort(function (a, b) {
      var av = a[t.sort.key], bv = b[t.sort.key];
      if (t.sort.key === 'acct' || t.sort.key === 'desc') return String(av || '').localeCompare(String(bv || '')) * t.sort.dir;
      return ((Number(av) || 0) - (Number(bv) || 0)) * t.sort.dir;
    });
    sorted.forEach(function (r) {
      lines.push([r.acct, r.desc].concat(keys.map(function (k) { return Number(r[k] || 0).toFixed(2); })).map(csvCell).join(','));
    });
    if (t.totals) lines.push(['Totals', ''].concat(keys.map(function (k) { return Number(t.totals[k] || 0).toFixed(2); })).map(csvCell).join(','));
    var blob = new Blob([lines.join('\r\n') + '\r\n'], { type: 'text/csv;charset=utf-8' });
    var url = URL.createObjectURL(blob);
    var a = mk('a', { href: url, download: 'tb_' + (overviewStem(S.overview) || 'engagement') + '_Y' + t.year + 'P' + t.period + '_' + t.group + '.csv' });
    document.body.appendChild(a);
    a.click();
    setTimeout(function () { URL.revokeObjectURL(url); a.remove(); }, 0);
  }

  /* -------------------------------------------------------------- documents */
  /* Two lenses over the same engagement:
   *
   *   Documents        — the CaseWare Document Manager: every physical file in
   *                      the engagement folder, LEFT-JOINed to its `SH` row.
   *                      Columns are CW's (name, class, schedule, title,
   *                      signoff, dates, size, lock/check-out).
   *   Register (SH)    — the `SH` table itself: the metadata index, in physical
   *                      CW document order.
   *
   * The grid is the truth: a row is a document if a file backs it OR the
   * register lists it. `backing` says which, and `join.tier` says how the pair
   * was matched, so an ambiguous or heuristic match is visible rather than
   * guessed at.
   */

  var DOCMAN_CLASSES = [
    { id: 'all', label: 'All classes' },
    { id: 'cvw', label: 'CaseView' },
    { id: 'word', label: 'Word' },
    { id: 'excel', label: 'Excel' },
    { id: 'pdf', label: 'PDF' },
    { id: 'image', label: 'Image' },
    { id: 'timevalue', label: 'TimeValue' },
    { id: 'other', label: 'Other file' },
    { id: 'internal', label: 'Internal (kernel)' }
  ];

  var JOIN_TIER_LABELS = {
    cvid: 'CVID',
    schedule: 'schedule',
    name: 'NAME tag',
    title: 'title',
    'schedule-prefix': 'schedule prefix',
    'title-contains': 'title contains',
    register: 'register',
    none: 'unmatched'
  };

  function docmanUrl(state) {
    var q = [];
    if (state.q) q.push('q=' + encodeURIComponent(state.q));
    if (state.klass && state.klass !== 'all') q.push('class=' + encodeURIComponent(state.klass));
    if (state.internals) q.push('internals=1');
    return '/api/docman' + (q.length ? '?' + q.join('&') : '');
  }

  async function loadDocman(state, force) {
    if (state.data && !force) return state.data;
    state.data = await getJSON(docmanUrl(state));
    return state.data;
  }

  function docExtClass(row) {
    var ext = text(row.ext).toLowerCase();
    return ext ? ext : 'file';
  }

  /* One grid row. `columns` is CW's Document Manager set. */
  function docmanRow(d, onSelect) {
    var sh = d.sh || null;
    var sel = S.docs.selected && S.docs.selected === d.id;
    var name = d.file || (sh ? (text(sh.title) || text(sh.schedule) || text(sh.guid) || ('SH row ' + text(sh.row))) : '(unnamed)');
    var title = sh ? text(sh.title) : '';
    var sched = sh ? text(sh.schedule) : '';
    var signoff = sh && sh.signoff ? sh.signoff : null;

    var signCell = mk('span', { class: 'small muted', text: '\u2014' });
    if (signoff) {
      var signCls = signoff.stage === 'reviewed' ? 'pill-ok'
        : signoff.stage === 'prepared' ? 'pill-warn'
          : 'pill-info';
      signCell = mk('span', {
        class: 'pill ' + signCls,
        title: signoff.stage + ' \u00b7 ' + signoff.signed + '/8 signed',
        text: signoff.stage === 'unsigned' ? 'unsigned' : signoff.stage + ' ' + signoff.signed + '/8'
      });
    } else if (!sh) {
      signCell = mk('span', { class: 'small muted', text: 'not in register' });
    } else {
      signCell = mk('span', { class: 'small muted', text: 'no signoff block' });
    }

    var lockCell = mk('span', { class: 'small muted', text: '\u2014' });
    if (sh && sh.flags) {
      var marks = [];
      if (sh.flags.lockdown) marks.push('locked');
      if (sh.flags.archive) marks.push('archived');
      if (sh.flags.roll) marks.push('rolled');
      if (sh.flags.hidden) marks.push('hidden');
      if (sh.flags.sign) marks.push('signed');
      if (marks.length) {
        lockCell = mk('span', {
          class: 'small',
          title: marks.join(', '),
          text: marks.join(' \u00b7 ')
        });
      }
    }

    var joinCell = mk('span', {
      class: 'small muted',
      title: d.join && d.join.tier ? ('join tier: ' + d.join.tier + (d.join.ambiguous ? ' (ambiguous)' : '')) : '',
      text: d.join ? (JOIN_TIER_LABELS[d.join.tier] || d.join.tier || '') : ''
    });
    if (d.join && d.join.ambiguous) {
      joinCell = mk('span', {
        class: 'pill pill-warn',
        title: 'more than one register row/file matched (heuristic join)',
        text: (JOIN_TIER_LABELS[d.join.tier] || d.join.tier) + ' ?'
      });
    }

    var row = mk('tr', {
      class: 'rowlink' + (sel ? ' sel' : ''),
      dataset: { id: d.id },
      title: d.rel || (sh ? 'SH row ' + text(sh.row) : ''),
      onclick: function () { onSelect(d); }
    }, [
      mk('td', { class: 'mono doc-name' }, [
        mk('span', { class: 'doc-dot k-' + docExtClass(d) }),
        mk('span', { text: name })
      ]),
      mk('td', { text: d.cw_class || d.format || '' }),
      mk('td', { class: 'mono small', text: sched }),
      mk('td', { class: 'desc', text: title }),
      mk('td', null, signCell),
      mk('td', { class: 'small mono', text: d.mtime_iso ? d.mtime_iso : '' }),
      mk('td', { class: 'num', text: d.bytes ? fmt.bytes(d.bytes) : '' }),
      mk('td', { class: 'small' }, lockCell),
      mk('td', { class: 'small' }, joinCell)
    ]);
    return row;
  }

  /* Tree data: directories from file relpaths + the SH heading hierarchy. */
  /* Bucket by folder. Register-only rows (no file on disk) get their own
     bucket so they are never mistaken for something in the engagement root. */
  var REGISTER_BUCKET = '\u0000register';

  function docmanTreeModel(d) {
    var rootsByDir = {};
    var order = [];
    d.rows.forEach(function (r) {
      var dir = r.file ? (r.dir || '') : REGISTER_BUCKET;
      if (!rootsByDir[dir]) { rootsByDir[dir] = []; order.push(dir); }
      rootsByDir[dir].push(r);
    });
    order.sort(function (a, b) {
      if (a === REGISTER_BUCKET) return 1;
      if (b === REGISTER_BUCKET) return -1;
      return a.localeCompare(b);
    });
    return { dirs: order, byDir: rootsByDir };
  }

  function renderDocmanTree(d, host, onSelect, onFilterDir) {
    var model = docmanTreeModel(d);
    var tree = mk('div', { class: 'tree doc-tree' });
    if (!model.dirs.length) {
      tree.appendChild(mk('div', { class: 'empty', text: 'No files match the current filter.' }));
      return tree;
    }
    model.dirs.forEach(function (dir) {
      var rows = model.byDir[dir] || [];
      var key = 'dir:' + dir;
      var collapsed = !!S.docs.collapsed[key];
      var label = dir === REGISTER_BUCKET
        ? 'Register only (no file on disk)'
        : dir === '' ? '(engagement root)' : dir;
      var head = mk('div', {
        class: 'tree-row dir-row' + (S.docs.dirFilter === dir ? ' sel' : ''),
        style: { paddingLeft: '4px' },
        title: label + ' \u2014 ' + rows.length + ' file(s)',
        onclick: function (ev) {
          if (ev.target && ev.target.classList.contains('tree-toggle')) {
            S.docs.collapsed[key] = !S.docs.collapsed[key];
            VIEWS.docs(document.getElementById('main-pane').firstChild);
            return;
          }
          onFilterDir(dir);
        }
      }, [
        mk('span', {
          class: 'tree-toggle',
          text: collapsed ? '\u25B8' : '\u25BE',
          title: collapsed ? 'Expand' : 'Collapse',
          onclick: function (ev) {
            ev.stopPropagation();
            S.docs.collapsed[key] = !S.docs.collapsed[key];
            VIEWS.docs(document.getElementById('main-pane').firstChild);
          }
        }),
        mk('span', { class: 'tree-title heading', text: label }),
        mk('span', { class: 'tree-meta' }, [mk('span', { text: String(rows.length) })])
      ]);
      tree.appendChild(head);
      if (collapsed) return;
      rows.slice(0, 400).forEach(function (r) {
        var sh = r.sh;
        var name = r.file || (sh ? (text(sh.title) || text(sh.schedule)) : '(row)');
        tree.appendChild(mk('div', {
          class: 'tree-row doc-leaf' + (S.docs.selected === r.id ? ' sel' : ''),
          style: { paddingLeft: '22px' },
          title: (r.rel || '') + (sh ? '  \u2192  ' + text(sh.schedule) : ''),
          onclick: function () { onSelect(r); }
        }, [
          mk('span', { class: 'tree-toggle leaf', text: '\u00b7' }),
          mk('span', { class: 'tree-title', text: name }),
          mk('span', { class: 'tree-meta' }, [
            mk('span', { text: text(r.ext) || 'sh' }),
            sh ? mk('span', { text: text(sh.schedule) }) : null
          ])
        ]));
      });
      if (rows.length > 400) {
        tree.appendChild(mk('div', {
          class: 'small muted',
          style: { paddingLeft: '22px' },
          text: '\u2026 ' + (rows.length - 400) + ' more in the grid'
        }));
      }
    });
    return tree;
  }

  /* Properties drawer: SH register fields + physical file facts. */
  function renderDocProperties(host, d) {
    var sh = d.sh || null;
    if (!d) return;
    var panel = mk('div', { class: 'panel doc-props' });
    var title = d.file || (sh ? text(sh.title) : '(document)');
    panel.appendChild(mk('h3', { class: 'panel-title', text: title }));

    var acts = mk('div', { class: 'btn-row', style: { marginBottom: '8px' } });
    if (d.rel) {
      acts.appendChild(mk('a', {
        class: 'btn btn-mini',
        href: d.open || ('/api/docman/file?path=' + encodeURIComponent(d.rel)),
        target: '_blank',
        rel: 'noopener',
        text: 'Open in new tab'
      }));
      if (d.preview === 'cvw') {
        acts.appendChild(mk('a', {
          class: 'btn btn-mini btn-primary',
          href: '/api/docman/preview-cvw?path=' + encodeURIComponent(d.rel),
          target: '_blank',
          rel: 'noopener',
          text: 'Preview sheet'
        }));
      } else if (d.preview === 'pdf' || d.preview === 'image') {
        acts.appendChild(mk('a', {
          class: 'btn btn-mini',
          href: d.open || ('/api/docman/file?path=' + encodeURIComponent(d.rel)),
          target: '_blank',
          rel: 'noopener',
          text: 'Preview inline'
        }));
      }
      acts.appendChild(mk('a', {
        class: 'btn btn-mini',
        href: d.open || ('/api/docman/file?path=' + encodeURIComponent(d.rel)),
        download: d.file || 'document',
        text: 'Download'
      }));
    }
    if (sh && sh.guid) {
      acts.appendChild(mk('button', {
        class: 'btn btn-mini',
        text: 'Signoff ladder',
        onclick: function () { S.signoff.doc = sh.guid; S.signoff.data = null; navigate('signoff'); }
      }));
    }
    panel.appendChild(acts);

    // File facts.
    var fileFacts = [];
    if (d.file) {
      fileFacts.push(['File', d.file]);
      fileFacts.push(['Folder', d.dir || '(engagement root)']);
      fileFacts.push(['Class', d.cw_class || '']);
      fileFacts.push(['Format', d.format || '']);
      fileFacts.push(['Size', fmt.bytes(d.bytes)]);
      fileFacts.push(['Modified', d.mtime_iso || '']);
      fileFacts.push(['MIME', d.mime || '']);
      if (d.sha256) fileFacts.push(['SHA-256', d.sha256]);
    } else {
      fileFacts.push(['File', '(no file on disk \u2014 register row)']);
      fileFacts.push(['Class', d.cw_class || '']);
    }
    if (d.join && d.join.tier) {
      fileFacts.push([
        'Register match',
        (JOIN_TIER_LABELS[d.join.tier] || d.join.tier) +
        (d.join.ambiguous ? ' \u00b7 AMBIGUOUS (' + d.join.candidates + ' candidates)' : '')
      ]);
    }
    panel.appendChild(kvTable(fileFacts));

    if (sh) {
      panel.appendChild(mk('h3', { class: 'panel-title', text: 'SH register' }));
      var regFacts = [
        ['Schedule', text(sh.schedule)],
        ['Title', text(sh.title)],
        ['Type', text(sh.type) + ' \u00b7 ' + text(sh.class)],
        ['PRG / NAME', text(sh.prg) + ' / ' + text(sh.name)],
        ['LINENO', sh.lineno === null || sh.lineno === undefined ? '' : fmt.int(sh.lineno)],
        ['CVID', text(sh.cvid)],
        ['Entity', text(sh.entity)],
        ['GUID', text(sh.guid)],
        ['Created', text(sh.created)],
        ['CW date', text(sh.cw_date)]
      ];
      var fl = sh.flags || {};
      var on = [];
      ['archive', 'roll', 'lockdown', 'hidden', 'delivered', 'sign'].forEach(function (k) {
        if (fl[k]) on.push(k);
      });
      regFacts.push(['Flags', on.length ? on.join(', ') : '(none set)']);
      panel.appendChild(kvTable(regFacts));

      var so = sh.signoff;
      panel.appendChild(mk('h3', { class: 'panel-title', text: 'Signoff' }));
      if (so) {
        panel.appendChild(kvTable([
          ['Stage', text(so.stage)],
          ['Signed levels', String(so.signed) + ' / 8'],
          ['Complete', so.complete ? 'yes' : 'no'],
          ['Next level', so.next_level === null || so.next_level === undefined ? '(none)' : String(so.next_level)]
        ]));
      } else {
        panel.appendChild(mk('div', { class: 'small muted', text: 'No signoff block on this row.' }));
      }
    } else {
      panel.appendChild(mk('div', {
        class: 'small muted',
        style: { marginTop: '8px' },
        text: 'This file has no SH row: CaseWare has not indexed it, so it carries no schedule, title or signoff. It is still part of the engagement file.'
      }));
    }
    host.replaceChildren(panel);
  }

  function countCard(k, v, hint) {
    return mk('div', { class: 'card' }, [
      mk('div', { class: 'k', text: k }),
      mk('div', { class: 'v', text: v === undefined || v === null ? '\u2014' : fmt.int(v) }),
      mk('div', { class: 'hint', text: hint || '' })
    ]);
  }

  function kvTable(pairs) {
    var body = mk('tbody');
    pairs.forEach(function (p) {
      if (p === null) return;
      body.appendChild(mk('tr', null, [
        mk('th', { text: p[0] }),
        mk('td', { class: 'mono', text: text(p[1]) })
      ]));
    });
    return mk('table', { class: 'kv' }, body);
  }

  /* Toolbar: filter, class, internals, lens switch, refresh. */
  function docmanToolbar(root, state, data) {
    var q = mk('input', {
      type: 'search',
      value: state.q || '',
      placeholder: 'Filter name, schedule, title, GUID\u2026',
      'aria-label': 'Filter documents',
      size: '30'
    });
    q.addEventListener('input', function () {
      state.q = q.value;
      var t;
      clearTimeout(t);
      t = setTimeout(function () {
        loadDocman(state, true).then(function () { VIEWS.docs(root); }).catch(toastErr);
      }, 260);
    });

    var cls = mk('select', { 'aria-label': 'Document class' });
    DOCMAN_CLASSES.forEach(function (c) {
      cls.appendChild(mk('option', { value: c.id, text: c.label, selected: (state.klass || 'all') === c.id }));
    });
    cls.addEventListener('change', function () {
      state.klass = cls.value;
      loadDocman(state, true).then(function () { VIEWS.docs(root); }).catch(toastErr);
    });

    var internals = mk('input', { type: 'checkbox', id: 'docman-internals' });
    internals.checked = !!state.internals;
    internals.addEventListener('change', function () {
      state.internals = internals.checked;
      loadDocman(state, true).then(function () { VIEWS.docs(root); }).catch(toastErr);
    });

    var counts = (data && data.counts) || {};
    var bar = mk('div', { class: 'toolbar doc-toolbar' }, [
      mk('span', { class: 'small muted', text: 'Lens' }),
      tabButton('Documents', state.lens === 'dms', function () {
        state.lens = 'dms';
        VIEWS.docs(root);
      }),
      tabButton('Register (SH)', state.lens === 'register', function () {
        state.lens = 'register';
        VIEWS.docs(root);
      }),
      mk('span', { class: 'sep' }),
      q,
      cls,
      mk('label', { class: 'small', style: { display: 'flex', gap: '4px', alignItems: 'center' } }, [
        internals,
        mk('span', { text: 'Show internals' })
      ]),
      mk('span', { class: 'sep' }),
      mk('span', {
        class: 'small muted',
        text: fmt.int(counts.files) + ' files \u00b7 ' + fmt.int(counts.documents) +
          ' documents \u00b7 ' + fmt.int(counts.joined) + ' joined \u00b7 ' +
          fmt.bytes(counts.bytes)
      }),
      mk('span', { style: { flex: '1' } }),
      mk('button', {
        class: 'btn', text: 'Refresh',
        onclick: function () { loadDocman(state, true).then(function () { VIEWS.docs(root); }).catch(toastErr); }
      })
    ]);
    return bar;
  }

  function tabButton(label, active, onclick) {
    return mk('button', {
      class: 'btn' + (active ? ' btn-primary' : ''),
      text: label,
      'aria-pressed': active ? 'true' : 'false',
      onclick: onclick
    });
  }

  /* Register lens: the SH table in physical CW document order. */
  async function renderRegisterLens(root, state) {
    var host = mk('div');
    root.appendChild(host);
    var d = state.regCache;
    if (!d) {
      host.appendChild(mk('div', { class: 'loading', text: 'Reading the SH register\u2026' }));
      try {
        d = await getJSON('/api/docs?mode=list');
        var census = null;
        try { census = await getJSON('/api/docs?mode=census'); } catch (e) { census = null; }
        state.regCache = { list: d, census: census };
      } catch (e) {
        host.replaceChildren(errorPane(e, function () { VIEWS.docs(root); }));
        toastErr(e);
        return;
      }
    }
    var rows = (d.list && d.list.rows) || [];
    var c = (d.census) || {};
    host.replaceChildren();

    host.appendChild(mk('div', { class: 'cards', style: { marginBottom: '10px' } }, [
      countCard('Register rows', c.live, 'live SH rows, decoded prefix'),
      countCard('Deleted', c.deleted, 'tombstones'),
      countCard('Headings', c.headings, 'tree headings'),
      countCard('CaseView forms', c.forms, 'response documents'),
      countCard('Analyses', c.analyses, 'analyses'),
      countCard('Blank LINENO', c.blank_lineno, 'unlinked rows'),
      countCard('Cycles', c.cycles, c.cycles ? 'tree defects' : 'none detected')
    ]));

    var tbody = mk('tbody');
    if (!rows.length) {
      tbody.appendChild(mk('tr', null, mk('td', { class: 'empty-cell', colspan: '7', text: 'The register has no live rows.' })));
    }
    rows.forEach(function (r) {
      var guid = text(r.guid).trim();
      tbody.appendChild(mk('tr', {
        class: 'rowlink',
        onclick: function () {
          if (!guid) { toast('This register row carries no GUID', 'err', 'no_guid'); return; }
          S.signoff.doc = guid;
          S.signoff.data = null;
          navigate('signoff');
        }
      }, [
        mk('td', { class: 'num', text: text(r.lineno) }),
        mk('td', { class: 'mono', text: text(r.prg) }),
        mk('td', { text: text(r.kind) }),
        mk('td', { class: 'mono', text: text(r.schedule) }),
        mk('td', { class: 'desc', text: text(r.title) || '(untitled)' }),
        mk('td', { class: 'mono small', text: guid })
      ]));
    });
    host.appendChild(mk('div', { class: 'grid-wrap' }, mk('table', { class: 'grid' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { class: 'num static', text: 'Line' }),
        mk('th', { class: 'static', text: 'PRG' }),
        mk('th', { class: 'static', text: 'Kind' }),
        mk('th', { class: 'static', text: 'Schedule' }),
        mk('th', { class: 'static', text: 'Title' }),
        mk('th', { class: 'static', text: 'GUID' })
      ])),
      tbody
    ])));
    host.appendChild(mk('div', {
      class: 'small muted',
      style: { marginTop: '6px' },
      text: fmt.int(rows.length) + ' live rows \u00b7 physical SH order (CW document order) \u00b7 click a row for its signoff ladder'
    }));
  }

  VIEWS.docs = async function (root) {
    var state = S.docs;
    root.replaceChildren();
    if (state.lens === 'register') {
      root.appendChild(viewHead('Document Manager', 'Register lens \u2014 the SH metadata index in CW document order'));
      await renderRegisterLens(root, state);
      return;
    }

    root.appendChild(viewHead('Document Manager',
      'Every document making up the file \u2014 the engagement folder joined to the SH register', [
        mk('button', {
          class: 'btn', text: 'Re-read from disk',
          onclick: function () { loadDocman(state, true).then(function () { VIEWS.docs(root); }).catch(toastErr); }
        })
      ]));

    var host = mk('div');
    root.appendChild(host);
    host.appendChild(mk('div', { class: 'loading', text: 'Reading the engagement folder\u2026' }));

    var data;
    try {
      data = await loadDocman(state, false);
    } catch (e) {
      host.replaceChildren(errorPane(e, function () { VIEWS.docs(root); }));
      toastErr(e);
      return;
    }
    host.replaceChildren();

    host.appendChild(docmanToolbar(root, state, data));

    var counts = data.counts || {};
    var sh = data.sh || {};
    var healthBits = [];
    if (sh.repaired) {
      healthBits.push('SH register repaired: ' + fmt.int(sh.live) + ' live rows from ' + fmt.int(sh.declared) + ' declared' + (sh.note ? ' (' + sh.note + ')' : ''));
    }

    host.appendChild(mk('div', { class: 'cards', style: { marginBottom: '10px' } }, [
      countCard('Documents', counts.documents, 'files + register rows'),
      countCard('Files on disk', counts.files, 'physical documents'),
      countCard('Joined to SH', counts.joined, 'file \u2194 register'),
      countCard('Unindexed', counts.unindexed, 'no SH row'),
      countCard('Register only', counts.register_only, 'no file on disk'),
      countCard('Ambiguous', counts.ambiguous, counts.ambiguous ? 'heuristic match' : 'none'),
      countCard('Internals', counts.internals, 'kernel / machinery')
    ]));

    if (healthBits.length) {
      host.appendChild(mk('div', {
        class: 'small',
        style: { marginBottom: '8px', padding: '6px 8px', background: 'var(--amber-bg)', border: '1px solid #ecd39b', borderRadius: '2px', color: '#7a5200' },
        text: healthBits.join(' \u00b7 ')
      }));
    }

    // Join law, stated (this is what the match actually did).
    var join = data.join || { tiers: [], misses: [] };
    var tierBits = [];
    (join.tiers || []).forEach(function (t) {
      if (t.hits > 0) tierBits.push(t.tier + '=' + t.hits);
    });
    (join.counts || {});
    if (tierBits.length) {
      host.appendChild(mk('div', {
        class: 'small muted',
        style: { marginBottom: '8px' },
        title: (join.tiers || []).map(function (t) { return t.tier + ': ' + t.law; }).join('\n'),
        text: 'Join tiers on this folder: ' + tierBits.join(' \u00b7 ') + ' \u2014 hover for the match law'
      }));
    }
    var misses = join.misses || [];
    if (misses.length) {
      var det = mk('details', { class: 'join-misses' }, [
        mk('summary', { class: 'small muted', text: 'Where the join misses (' + misses.length + ')' })
      ]);
      var ul = mk('ul');
      misses.forEach(function (m) { ul.appendChild(mk('li', { text: m })); });
      det.appendChild(ul);
      host.appendChild(det);
    }

    if (!data.rows || !data.rows.length) {
      host.appendChild(mk('div', {
        class: 'empty',
        text: 'No documents match the current filter. Clear the filter, widen the class, or tick Show internals.'
      }));
      return;
    }

    // Split: tree (folders) | grid + drawer.
    var treeHost = mk('div', { class: 'panel doc-tree-panel' });
    var gridHost = mk('div', { class: 'doc-grid-panel' });
    var drawer = mk('div', { class: 'panel doc-drawer' });
    var split = mk('div', { class: 'split split-wide docman-split' }, [treeHost, mk('div', null, [gridHost, drawer])]);
    host.appendChild(split);

    var visible = data.rows.filter(function (r) {
      if (!state.dirFilter) return true;
      if (state.dirFilter === REGISTER_BUCKET) return !r.file;
      if (!r.file) return false;
      return (r.dir || '') === state.dirFilter;
    });

    function select(d) {
      state.selected = d.id;
      Array.prototype.forEach.call(gridHost.querySelectorAll('tr.rowlink'), function (tr) {
        tr.classList.toggle('sel', tr.dataset.id === d.id);
      });
      renderDocProperties(drawer, d);
    }

    treeHost.appendChild(renderDocmanTree(
      { rows: data.rows },
      treeHost,
      select,
      function (dir) {
        state.dirFilter = state.dirFilter === dir ? null : dir;
        VIEWS.docs(root);
      }
    ));

    // CW's Document Manager columns.
    var tbody = mk('tbody');
    visible.forEach(function (r) { tbody.appendChild(docmanRow(r, select)); });
    gridHost.appendChild(mk('div', { class: 'grid-wrap doc-grid-wrap' }, mk('table', { class: 'grid doc-grid' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { class: 'static', text: 'Name' }),
        mk('th', { class: 'static', text: 'Class' }),
        mk('th', { class: 'static', text: 'Schedule' }),
        mk('th', { class: 'static', text: 'Title' }),
        mk('th', { class: 'static', text: 'Signoff' }),
        mk('th', { class: 'static', text: 'Modified' }),
        mk('th', { class: 'num static', text: 'Size' }),
        mk('th', { class: 'static', text: 'Lock / flags' }),
        mk('th', { class: 'static', text: 'SH match' })
      ])),
      tbody
    ])));

    var sel = null;
    if (state.selected) {
      sel = visible.filter(function (r) { return r.id === state.selected; })[0] || null;
    }
    if (sel) {
      Array.prototype.forEach.call(gridHost.querySelectorAll('tr.rowlink'), function (tr) {
        tr.classList.toggle('sel', tr.dataset.id === sel.id);
      });
      renderDocProperties(drawer, sel);
    } else {
      drawer.replaceChildren(mk('div', {
        class: 'small muted',
        text: 'Select a row to see its register fields, file facts and open/preview actions.'
      }));
    }

    var foot = [];
    foot.push(fmt.int(visible.length) + ' row(s) shown');
    if (state.dirFilter) {
      foot.push('folder: ' + (state.dirFilter === REGISTER_BUCKET ? 'register only' : state.dirFilter || '(root)'));
    }
    if (state.q) foot.push('filter: ' + state.q);
    if (state.klass && state.klass !== 'all') foot.push('class: ' + state.klass);
    if (data.counts && data.counts.rows !== visible.length) {
      foot.push('of ' + fmt.int(data.counts.rows) + ' in this lens');
    }
    host.appendChild(mk('div', { class: 'small muted', style: { marginTop: '6px' }, text: foot.join(' \u00b7 ') }));
  };

  /* --------------------------------------------------------------- accounts */

  async function loadAccountIndex() {
    if (S.accounts.rows.length) return S.accounts.rows;
    var data = await getJSON('/api/tb?year=0&period=12&group=A&limit=0');
    S.accounts.rows = (data.rows || []).map(function (r) { return { acct: text(r.acct), desc: text(r.desc), ytd: r.ytd }; });
    return S.accounts.rows;
  }

  VIEWS.accounts = async function (root) {
    root.replaceChildren();
    root.appendChild(viewHead('Account Properties', 'AM chart of accounts \u2014 12 property fields in dialog order, with balances'));

    var host = mk('div');
    root.appendChild(host);
    try {
      await loadAccountIndex();
    } catch (e) {
      host.appendChild(errorPane(e, function () { VIEWS.accounts(root); }));
      toastErr(e);
      return;
    }
    var a = S.accounts;
    if (!a.selected && a.rows.length) a.selected = a.rows[0].acct;

    var search = mk('input', { type: 'search', placeholder: 'Filter accounts\u2026', value: a.q, 'aria-label': 'filter accounts', style: { width: '100%' } });
    search.addEventListener('input', function () { a.q = search.value; renderList(); });

    var listBody = mk('div', { class: 'listbox', id: 'acct-list' });
    function renderList() {
      listBody.replaceChildren();
      var q = a.q.trim().toLowerCase();
      var filtered = a.rows.filter(function (r) {
        return !q || r.acct.toLowerCase().indexOf(q) >= 0 || r.desc.toLowerCase().indexOf(q) >= 0;
      }).slice(0, 800);
      if (!filtered.length) {
        listBody.appendChild(mk('div', { class: 'empty', style: { border: 'none' }, text: 'No account matches that filter.' }));
        return;
      }
      filtered.forEach(function (r) {
        listBody.appendChild(mk('div', {
          class: 'row' + (a.selected === r.acct ? ' sel' : ''),
          onclick: function () { a.selected = r.acct; a.detail = null; renderList(); loadDetail(); }
        }, [
          mk('span', { class: 'a', text: r.acct }),
          mk('span', { class: 'd', text: r.desc || '(no description)', title: r.desc })
        ]));
      });
    }

    var detailHost = mk('div');
    /* Clicking two accounts quickly must show the second one: only the newest
       request is allowed to paint. */
    var detailSeq = 0;
    async function loadDetail() {
      if (!a.selected) { detailHost.replaceChildren(mk('div', { class: 'empty', text: 'Select an account.' })); return; }
      var want = a.selected;
      var seq = ++detailSeq;
      detailHost.replaceChildren(mk('div', { class: 'loading', text: 'Loading account ' + want + '\u2026' }));
      try {
        var data = await getJSON('/api/account/' + encodeURIComponent(want));
        if (seq !== detailSeq) return;
        a.detail = data;
        detailHost.replaceChildren(renderAccount(data));
      } catch (e) {
        if (seq !== detailSeq) return;
        detailHost.replaceChildren(errorPane(e, loadDetail));
        toastErr(e);
      }
    }

    renderList();
    host.appendChild(mk('div', { class: 'split' }, [
      mk('div', null, [search, mk('div', { style: { height: '6px' } }), listBody]),
      detailHost
    ]));
    loadDetail();
  };

  function renderAccount(d) {
    if (!d) return mk('div', { class: 'empty', text: 'No account data.' });
    var wrap = mk('div');
    wrap.appendChild(mk('div', { class: 'panel' }, [
      mk('div', { class: 'panel-title', text: text(d.acct) + '  \u00b7  ' + text(d.desc) }),
      mk('table', { class: 'kv' }, mk('tbody', null, [
        mk('tr', null, [mk('th', { text: 'Type' }), mk('td', { text: text(d.type) })]),
        mk('tr', null, [mk('th', { text: 'Class' }), mk('td', { text: text(d['class']) })]),
        mk('tr', null, [mk('th', { text: 'Schedule' }), mk('td', { text: text(d.schedule) || '\u2014' })]),
        mk('tr', null, [mk('th', { text: 'Entity' }), mk('td', { text: text(d.entity) || '(blank)' })]),
        mk('tr', null, [mk('th', { text: 'GUID' }), mk('td', { class: 'mono', text: text(d.guid) })])
      ]))
    ]));

    var props = d.properties || {};
    var order = ['POST', 'PRINT', 'CTL', 'TOTAL', 'SIGN', 'DC', 'COLUMN', 'DOLLAR', 'ULINE', 'PAGE', 'DEN', 'HIDDEN'];
    var keys = Object.keys(props);
    keys.sort(function (x, y) {
      var ix = order.indexOf(x), iy = order.indexOf(y);
      if (ix < 0) ix = 99;
      if (iy < 0) iy = 99;
      return ix - iy || x.localeCompare(y);
    });
    var pbody = mk('tbody');
    keys.forEach(function (k) {
      var v = props[k];
      pbody.appendChild(mk('tr', null, [
        mk('th', { text: k }),
        mk('td', { class: 'mono', text: text(v) === '' ? '(blank)' : text(v) })
      ]));
    });
    if (!keys.length) pbody.appendChild(mk('tr', null, mk('td', { colspan: '2', class: 'muted', text: 'No property fields returned.' })));

    wrap.appendChild(mk('h2', { class: 'section', text: 'Properties (dialog order)' }));
    wrap.appendChild(mk('div', { class: 'panel' }, mk('table', { class: 'kv' }, pbody)));

    var bal = [
      ['Opening', d.open], ['Month', d.mth], ['YTD', d.ytd],
      ['AJE', d.aje], ['EJE', d.eje], ['RJE', d.rje]
    ];
    var brow = mk('tr');
    bal.forEach(function (b) {
      brow.appendChild(mk('th', { text: b[0], style: { width: 'auto' } }));
    });
    var brow2 = mk('tr');
    bal.forEach(function (b) { brow2.appendChild(mk('td', { class: 'num right', text: fmt.num(b[1]) })); });
    wrap.appendChild(mk('h2', { class: 'section', text: 'Balances' }));
    wrap.appendChild(mk('div', { class: 'panel' }, mk('table', { class: 'kv' }, mk('tbody', null, [brow, brow2]))));

    var groups = d.groups || [];
    if (groups.length) {
      var gbody = mk('tbody');
      groups.forEach(function (g) {
        gbody.appendChild(mk('tr', null, [
          mk('th', { text: 'Scheme ' + text(g.scheme) }),
          mk('td', { class: 'mono', text: text(g.node) })
        ]));
      });
      wrap.appendChild(mk('h2', { class: 'section', text: 'Grouping assignments' }));
      wrap.appendChild(mk('div', { class: 'panel' }, mk('table', { class: 'kv' }, gbody)));
    }

    wrap.appendChild(mk('div', { class: 'btn-row', style: { marginTop: '10px' } }, [
      mk('button', {
        class: 'btn', text: 'Trial balance row',
        onclick: function () { S.tb.entity = ''; navigate('tb'); }
      }),
      mk('button', {
        class: 'btn', text: 'Entries for this account',
        onclick: function () { S.entries.acct = text(d.acct); S.entries.list = []; navigate('entries'); }
      })
    ]));
    return wrap;
  }

  /* ---------------------------------------------------------------- entries */

  function entryTypeOptions(selected) {
    var types = [
      ['N', 'Adjusting entry'], ['R', 'Reclassifying entry'], ['U', 'Unrecorded proposed entry'],
      ['E', 'Eliminating entry'], ['T', 'Tax - federal'], ['V', 'Tax - state'], ['C', 'Tax - city'],
      ['B', 'Other basis'], ['L', 'Unrecorded projected entry'], ['G', 'Unrecorded judgmental entry'],
      ['2', 'Other 2'], ['3', 'Other 3']
    ];
    return types.map(function (t) {
      return mk('option', { value: t[0], text: t[1] + '  (' + t[0] + ')' });
    });
  }

  async function loadEntries() {
    var q = '/api/entries';
    if (S.entries.acct) q += '?acct=' + encodeURIComponent(S.entries.acct);
    var data = await getJSON(q);
    S.entries.list = data.entries || data.rows || [];
    S.entries.count = data.count === undefined || data.count === null ? S.entries.list.length : Number(data.count);
    return S.entries.list;
  }

  VIEWS.entries = async function (root) {
    root.replaceChildren();
    root.appendChild(viewHead('Adjusting Entries', 'GL rows grouped by entry (header + lines, EntryType decoded)', [
      mk('button', { class: 'btn', text: 'New entry', onclick: function () { navigate('operations'); } })
    ]));

    var acct = mk('input', { type: 'text', value: S.entries.acct, placeholder: 'all accounts', size: '12', 'aria-label': 'account filter' });
    var apply = mk('button', { class: 'btn btn-primary', text: 'Load' });
    var reload = function () {
      S.entries.acct = acct.value.trim();
      S.entries.selected = '';
      VIEWS.entries(root);
    };
    apply.addEventListener('click', reload);
    acct.addEventListener('keydown', function (e) { if (e.key === 'Enter') reload(); });
    root.appendChild(mk('div', { class: 'toolbar' }, [
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Account' }), acct]),
      apply,
      mk('span', { class: 'sep' }),
      mk('span', { class: 'small muted', text: S.entries.acct ? 'JOURSOURCE partition, entry view' : 'All live AJ entries' }),
      mk('span', { class: 'sep' }),
      mk('span', { class: 'small muted', id: 'entry-count', text: '' })
    ]));

    var host = mk('div');
    root.appendChild(host);
    try {
      await loadEntries();
    } catch (e) {
      host.appendChild(errorPane(e, reload));
      toastErr(e);
      return;
    }

    var list = S.entries.list;
    var countEl = document.getElementById('entry-count');
    if (countEl) {
      countEl.textContent = fmt.int(list.length) +
        (S.entries.count !== null && S.entries.count !== list.length ? ' of ' + fmt.int(S.entries.count) : '') +
        (list.length === 1 ? ' entry' : ' entries');
    }
    if (!list.length) {
      host.appendChild(mk('div', { class: 'empty', text: 'No adjusting entries' + (S.entries.acct ? ' touch account ' + S.entries.acct : '') + '.' }));
      return;
    }
    if (!S.entries.selected) S.entries.selected = text(list[0].ref || list[0].system_id);

    var rowsBody = mk('tbody');
    var detail = mk('div');
    function sel(ref) {
      S.entries.selected = ref;
      Array.prototype.forEach.call(rowsBody.children, function (tr) {
        tr.classList.toggle('sel', tr.dataset.ref === ref);
        tr.style.background = tr.dataset.ref === ref ? 'var(--blue-100)' : '';
      });
      var e = list.filter(function (x) { return text(x.ref || x.system_id) === ref; })[0];
      detail.replaceChildren(renderEntryDetail(e));
    }
    list.forEach(function (e) {
      var ref = text(e.ref || e.system_id);
      var balanced = e.balanced === undefined ? null : !!e.balanced;
      var tr = mk('tr', {
        class: 'rowlink',
        dataset: { ref: ref },
        onclick: function () { sel(ref); }
      }, [
        mk('td', { class: 'mono', text: ref }),
        mk('td', { class: 'mono small', text: text(e.date) }),
        mk('td', { text: text(e.type_label || e.type || e.type_code) }),
        mk('td', { class: 'desc', text: text(e.desc) || '(no description)', title: text(e.desc) }),
        mk('td', { class: 'num', text: fmt.num(e.total) }),
        mk('td', null, balanced === null
          ? mk('span', { class: 'pill', text: 'n/a' })
          : mk('span', { class: 'pill ' + (balanced ? 'pill-ok' : 'pill-bad'), text: balanced ? 'balanced' : 'out of balance' })),
        mk('td', { class: 'mono small', text: text(e.journal || e.journal_source) }),
        mk('td', { class: 'num', text: e.lines ? e.lines.length : text(e.line_count) })
      ]);
      rowsBody.appendChild(tr);
    });

    var left = mk('div', { class: 'grid-wrap', style: { maxHeight: 'calc(100vh - 250px)' } }, mk('table', { class: 'grid compact' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { class: 'static', text: 'Ref' }),
        mk('th', { class: 'static', text: 'Date' }),
        mk('th', { class: 'static', text: 'Type' }),
        mk('th', { class: 'static', text: 'Description' }),
        mk('th', { class: 'num static', text: 'Amount' }),
        mk('th', { class: 'static', text: 'Balance' }),
        mk('th', { class: 'static', text: 'Journal' }),
        mk('th', { class: 'num static', text: 'Lines' })
      ])),
      rowsBody
    ]));

    /* The entry list carries eight columns, so it gets the wider half. */
    host.appendChild(mk('div', { class: 'split split-2-1' }, [left, detail]));
    sel(S.entries.selected);
  };

  function renderEntryDetail(e) {
    if (!e) return mk('div', { class: 'empty', text: 'Select an entry.' });
    var lines = e.lines || [];
    var lbody = mk('tbody');
    lines.forEach(function (l) {
      lbody.appendChild(mk('tr', null, [
        mk('td', { class: 'mono', text: text(l.acct) }),
        mk('td', { class: 'desc', text: text(l.desc), title: text(l.desc) }),
        mk('td', { class: 'mono small', text: text(l.type_label || l.type_code) }),
        mk('td', { class: 'num', text: fmt.num(l.amount) })
      ]));
    });
    if (!lines.length) lbody.appendChild(mk('tr', null, mk('td', { class: 'empty-cell', colspan: '4', text: 'No lines returned for this entry.' })));

    var balanced = e.balanced === undefined ? null : !!e.balanced;
    return mk('div', { class: 'panel' }, [
      mk('div', { class: 'panel-title', text: 'Entry ' + text(e.ref || e.system_id) }),
      mk('table', { class: 'kv', style: { marginBottom: '8px' } }, mk('tbody', null, [
        mk('tr', null, [mk('th', { text: 'Date' }), mk('td', { class: 'mono', text: text(e.date) })]),
        mk('tr', null, [mk('th', { text: 'Description' }), mk('td', { text: text(e.desc) || '\u2014' })]),
        mk('tr', null, [mk('th', { text: 'Type' }), mk('td', { text: text(e.type_label || e.type) + (e.type_code !== undefined ? '  (' + e.type_code + ')' : '') })]),
        mk('tr', null, [mk('th', { text: 'Period' }), mk('td', { text: text(e.period) || '\u2014' })]),
        mk('tr', null, [mk('th', { text: 'Balance' }), mk('td', null,
          balanced === null ? mk('span', { class: 'pill', text: 'unknown' })
            : mk('span', { class: 'pill ' + (balanced ? 'pill-ok' : 'pill-bad'), text: balanced ? 'balanced' : 'difference ' + fmt.num(e.difference) }))]),
        mk('tr', null, [mk('th', { text: 'Journal' }), mk('td', { class: 'mono', text: text(e.journal || e.journal_source) })])
      ])),
      mk('table', { class: 'grid compact' }, [
        mk('thead', null, mk('tr', null, [
          mk('th', { class: 'static', text: 'Account' }),
          mk('th', { class: 'static', text: 'Description' }),
          mk('th', { class: 'static', text: 'Type' }),
          mk('th', { class: 'num static', text: 'Amount' })
        ])),
        lbody
      ])
    ]);
  }

  /* ----------------------------------------------------------------- signoff */

  var ROLE_FALLBACK = ['Prepared by 1', 'Prepared by 2', 'Prepared by 3', 'Reviewed by 1', 'Reviewed by 2', 'Reviewed by 3', 'Reviewed by 4', 'Reviewed by 5'];

  async function loadSignoff() {
    if (!S.signoff.doc) return null;
    var data = await getJSON('/api/signoff?doc=' + encodeURIComponent(S.signoff.doc));
    S.signoff.data = data;
    return data;
  }

  VIEWS.signoff = async function (root) {
    root.replaceChildren();
    root.appendChild(viewHead('Signoff', 'Eight-level ladder (3 prepared, 5 reviewed) per SH document'));

    var docs = mk('datalist', { id: 'doc-list' });
    var docIn = mk('input', { type: 'text', list: 'doc-list', value: S.signoff.doc, placeholder: 'guid or schedule', size: '40', 'aria-label': 'document' });
    var apply = mk('button', { class: 'btn btn-primary', text: 'Load' });
    var reload = function () { S.signoff.doc = docIn.value.trim(); VIEWS.signoff(root); };
    apply.addEventListener('click', reload);
    docIn.addEventListener('keydown', function (e) { if (e.key === 'Enter') reload(); });

    var quick = mk('select', { 'aria-label': 'document picker' });
    quick.appendChild(mk('option', { value: '', text: 'Pick a document\u2026' }));
    try {
      var listData = await getJSON('/api/docs?mode=list');
      S.docs.list = listData.rows || [];
    } catch (e) { S.docs.list = []; }
    S.docs.list.forEach(function (r) {
      if (!r.guid) return;
      var label = (text(r.schedule) ? text(r.schedule) + '  ' : '') + (text(r.title) || '(untitled)');
      quick.appendChild(mk('option', { value: text(r.guid), text: label.slice(0, 100) }));
      docs.appendChild(mk('option', { value: text(r.guid), label: label }));
    });
    quick.addEventListener('change', function () {
      if (!quick.value) return;
      docIn.value = quick.value;
      S.signoff.doc = quick.value;
      VIEWS.signoff(root);
    });

    root.appendChild(mk('div', { class: 'toolbar' }, [
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Document' }), docIn, docs]),
      apply,
      mk('span', { class: 'sep' }),
      quick,
      mk('span', { class: 'sep' }),
      mk('button', { class: 'btn', text: 'Signoff status list', onclick: function () { navigate('docs'); } })
    ]));

    var host = mk('div');
    root.appendChild(host);

    if (!S.signoff.doc) {
      host.appendChild(mk('div', { class: 'empty', text: 'Pick a document above (or from the Documents tree) to see its signoff ladder.' }));
      return;
    }
    try {
      await loadSignoff();
    } catch (e) {
      host.appendChild(errorPane(e, function () { VIEWS.signoff(root); }));
      toastErr(e);
      return;
    }
    var d = S.signoff.data || {};
    var doc = d.doc || d.document || {};
    var levels = d.levels || [];
    var highest = d.highest_signed;
    var complete = d.complete;

    var summary = mk('div', { class: 'cards', style: { marginBottom: '10px' } }, [
      mk('div', { class: 'card' }, [
        mk('div', { class: 'k', text: 'Stage' }),
        mk('div', { class: 'v', text: text(d.stage) || '\u2014' }),
        mk('div', { class: 'hint', text: complete ? 'ladder complete' : 'in progress' })
      ]),
      mk('div', { class: 'card ' + (complete ? '' : 'warn') }, [
        mk('div', { class: 'k', text: 'Highest signed' }),
        mk('div', { class: 'v', text: highest === undefined || highest === null ? '\u2014' : 'Level ' + highest }),
        mk('div', { class: 'hint', text: highest ? roleLabel(highest) : 'unsigned' })
      ]),
      mk('div', { class: 'card' }, [
        mk('div', { class: 'k', text: 'Progress' }),
        mk('div', { class: 'v', text: levels.filter(function (l) { return l.signed; }).length + ' / 8' }),
        mk('div', { class: 'hint', text: 'levels signed' })
      ])
    ]);
    host.appendChild(summary);

    host.appendChild(mk('div', { class: 'panel', style: { marginBottom: '10px' } }, [
      mk('div', { class: 'panel-title', text: text(doc.title) || text(doc.schedule) || text(doc.guid) || S.signoff.doc }),
      mk('table', { class: 'kv' }, mk('tbody', null, [
        mk('tr', null, [mk('th', { text: 'GUID' }), mk('td', { class: 'mono', text: text(doc.guid) || S.signoff.doc })]),
        mk('tr', null, [mk('th', { text: 'Schedule' }), mk('td', { text: text(doc.schedule) || '\u2014' })]),
        mk('tr', null, [mk('th', { text: 'Stage' }), mk('td', null, mk('span', { class: 'pill ' + (complete ? 'pill-ok' : (d.stage ? 'pill-info' : '')), text: text(d.stage) || 'unsigned' }))])
      ]))
    ]));

    if (!levels.length) {
      host.appendChild(mk('div', { class: 'empty', text: 'This document has no signoff block (all eight levels blank).' }));
      return;
    }

    var tl = mk('ul', { class: 'timeline' });
    levels.forEach(function (l) {
      var n = Number(l.level) || 0;
      var signed = !!l.signed;
      var isCurrent = !signed && (highest === undefined || highest === null ? n === 1 : n === Number(highest) + 1);
      tl.appendChild(mk('li', { class: 'tl-item ' + (signed ? 'done' : (isCurrent ? 'current' : '')) }, [
        mk('div', { class: 'tl-dot', text: signed ? '\u2713' : String(n) }),
        mk('div', { class: 'tl-role', text: text(l.role) || roleLabel(n) }),
        mk('div', { class: 'tl-meta' }, [
          mk('span', { text: signed ? 'signed' : (isCurrent ? 'next to sign' : 'pending') }),
          l.initials ? mk('span', { class: 'mono', text: 'initials ' + text(l.initials) }) : null,
          l.user ? mk('span', { text: text(l.user) }) : null,
          l.date ? mk('span', { class: 'mono', text: text(l.date) }) : null,
          l.signature_guid ? mk('span', { class: 'mono', text: 'sig ' + fmt.sha(l.signature_guid) }) : null
        ])
      ]));
    });
    host.appendChild(tl);
  };

  function roleLabel(n) {
    return ROLE_FALLBACK[Number(n) - 1] || ('Level ' + n);
  }

  /* --------------------------------------------------------------- operations */

  function auditAdd(op, data, ok, errMsg) {
    S.ops.audit.unshift({
      t: new Date(),
      op: op,
      ok: ok,
      detail: ok ? summarizeOp(data) : errMsg || 'failed'
    });
    renderAudit();
  }

  function summarizeOp(data) {
    if (!data) return '';
    var bits = [];
    var r = data.result || data.preview || {};
    if (r.cells !== undefined) bits.push(fmt.int(r.cells) + ' cells');
    if (r.cells_changed !== undefined) bits.push(fmt.int(r.cells_changed) + ' cells');
    if (Array.isArray(r.tables)) bits.push(r.tables.length + ' tables');
    if (r.backup) bits.push('backup ' + r.backup);
    if (data.backup) bits.push('backup ' + data.backup);
    return bits.join(' \u00b7 ') || 'ok';
  }

  var auditHost = null;

  function renderAudit() {
    if (!auditHost) return;
    var body = mk('tbody');
    if (!S.ops.audit.length) {
      body.appendChild(mk('tr', null, mk('td', { colspan: '4', class: 'muted', text: 'No commits in this session.' })));
    }
    S.ops.audit.forEach(function (a) {
      body.appendChild(mk('tr', { class: a.ok ? '' : 'fail' }, [
        mk('td', { class: 't', text: a.t.toLocaleTimeString('en-US', { hour12: false }) }),
        mk('td', { text: a.op }),
        mk('td', null, mk('span', { class: 'pill ' + (a.ok ? 'pill-ok' : 'pill-bad'), text: a.ok ? 'committed' : 'failed' })),
        mk('td', { class: 'small', text: a.detail })
      ]));
    });
    auditHost.replaceChildren(mk('table', { class: 'audit' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { text: 'Time' }), mk('th', { text: 'Operation' }), mk('th', { text: 'Verdict' }), mk('th', { text: 'Detail' })
      ])),
      body
    ]));
  }

  async function opCall(kind, phase, body) {
    var parts = kind.split('/');
    var paths;
    if (parts[0] === 'lifecycle') {
      paths = ['/api/lifecycle/' + parts[1] + '/' + phase, '/api/lifecycle/' + parts[1]];
    } else {
      paths = ['/api/' + kind + '/' + phase];
    }
    var last = null;
    for (var i = 0; i < paths.length; i++) {
      var p = paths[i];
      /* The suffixed routes carry the phase in the path; the unsuffixed
         lifecycle route is phase-selected in the body, so name the phase three
         ways rather than guessing which flag the server reads. */
      var payload = /\/preview$|\/commit$/.test(p)
        ? body
        : Object.assign({ mode: phase, commit: phase === 'commit', preview: phase === 'preview' }, body);
      try {
        return await postJSON(p, payload);
      } catch (e) {
        last = e;
        if (e && (e.status === 404 || e.status === 405)) continue;
        throw e;
      }
    }
    throw last;
  }

  function opCard(opts) {
    var out = mk('div', { class: 'op-out' });
    var previewBtn = mk('button', { class: 'btn', text: '1  Preview' });
    var commitBtn = mk('button', { class: 'btn btn-primary', text: '2  Commit', disabled: true });
    var status = mk('span', { class: 'step', text: 'preview \u2192 commit' });
    var pending = null;

    /* A preview is a promise about one exact spec. Editing the form after the
       preview retires it, so Commit can never write something the user did not
       see. (Commit also sends the frozen previewed body, never live fields.) */
    function invalidate() {
      if (!pending) return;
      pending = null;
      commitBtn.disabled = true;
      previewBtn.textContent = '1  Preview';
      status.textContent = 'form changed \u00b7 preview again';
    }
    opts.form.addEventListener('input', invalidate);
    opts.form.addEventListener('change', invalidate);

    previewBtn.addEventListener('click', async function () {
      var body;
      pending = null;
      commitBtn.disabled = true;
      try {
        body = opts.body();
      } catch (e) {
        status.textContent = 'input rejected \u00b7 nothing sent';
        toast(e.message, 'err', 'input');
        return;
      }
      previewBtn.disabled = true;
      commitBtn.disabled = true;
      previewBtn.textContent = '1  Preview';
      status.textContent = 'computing preview\u2026';
      out.replaceChildren(mk('div', { class: 'loading', text: 'Computing preview\u2026' }));
      try {
        var data = await opCall(opts.kind, 'preview', body);
        pending = { body: body, data: data };
        out.replaceChildren.apply(out, renderOpResult(opts.kind, data, 'preview'));
        status.textContent = 'preview ready \u00b7 nothing written';
        commitBtn.disabled = false;
      } catch (e) {
        /* A refusal is an answer: render the payload the server computed
           alongside the error instead of hiding it behind a banner. */
        var refused = e.body && e.body.preview;
        if (refused) {
          out.replaceChildren.apply(out, [
            mk('div', { class: 'btn-row', style: { marginBottom: '6px' } }, [
              mk('span', { class: 'pill pill-bad', text: 'refused' }),
              mk('span', { class: 'pill pill-mono', text: e.code })
            ])
          ].concat(renderOpResult(opts.kind, e.body, 'preview')));
        } else {
          out.replaceChildren(errorPane(e));
        }
        status.textContent = 'preview refused \u00b7 nothing written';
        toastErr(e);
      } finally {
        previewBtn.disabled = false;
      }
    });

    commitBtn.addEventListener('click', async function () {
      if (!pending) return;
      previewBtn.disabled = true;
      commitBtn.disabled = true;
      status.textContent = 'committing\u2026';
      out.replaceChildren(mk('div', { class: 'loading', text: 'Committing through cw_core::transact\u2026' }));
      try {
        /* If the preview pinned a token (server-side guard that the engagement
           has not moved since), send it back so the commit can refuse a stale
           preview instead of writing unreviewed output. */
        var commitBody = pending.body;
        var tok = pending.data && (pending.data.token || pending.data.preview_token);
        if (tok) commitBody = Object.assign({ token: tok, preview_token: tok }, commitBody);
        var data = await opCall(opts.kind, 'commit', commitBody);
        out.replaceChildren.apply(out, renderOpResult(opts.kind, data, 'commit'));
        status.textContent = 'committed \u00b7 preview again to write more';
        auditAdd(opts.kind, data, true);
        toast(opts.title + ': committed', 'ok');
        /* A committed preview is spent: require a fresh preview before the
           same form can write again, so no double-commit is one click away. */
        pending = null;
        previewBtn.textContent = '1  Preview again';
        invalidateCaches();
        refreshOverview(true);
      } catch (e) {
        out.replaceChildren(errorPane(e));
        status.textContent = 'commit failed';
        toastErr(e);
        auditAdd(opts.kind, null, false, e.message + (e.code ? ' [' + e.code + ']' : ''));
        commitBtn.disabled = false;
      } finally {
        previewBtn.disabled = false;
      }
    });

    return mk('div', { class: 'opcard' }, [
      mk('h3', { text: opts.title }),
      mk('div', { class: 'blurb', text: opts.blurb }),
      opts.form,
      mk('div', { class: 'stepbar' }, [status, mk('span', { style: { flex: '1' } }), previewBtn, commitBtn]),
      out
    ]);
  }

  function renderOpResult(kind, data, phase) {
    var nodes = [];
    var head = mk('div', { class: 'btn-row', style: { marginBottom: '6px' } }, [
      mk('span', { class: 'pill ' + (phase === 'commit' ? 'pill-ok' : 'pill-info'), text: phase }),
      data && data.ok === false ? mk('span', { class: 'pill pill-bad', text: 'rejected' }) : null,
      data && data.backup ? mk('span', { class: 'pill pill-warn pill-mono', text: 'backup ' + data.backup }) : null
    ]);
    nodes.push(head);

    var payload = (data && (data.preview || data.result)) || {};
    var opName = (data && data.op) || kind;

    var specific = renderOpSpecific(opName, kind, payload);
    if (specific) nodes.push(specific);

    var manifest = data && (data.manifest || data.manifest_diff || data.diff);
    if (manifest) {
      var diff = renderDiff(manifest);
      if (diff) nodes.push(mk('h4', { class: 'panel-title', style: { marginTop: '8px' }, text: 'Manifest diff (before \u2192 after)' }), diff);
    }

    /* Op-specific renderers consume the fields they know; anything else the
       server adds is still shown, so a new field is never invisible. */
    var leftovers = specific ? prunePayload(payload) : null;
    if (!specific) nodes.push(renderGeneric(payload));
    else if (leftovers) nodes.push(renderGeneric(leftovers));
    return nodes;
  }

  function prunePayload(p) {
    if (!p || typeof p !== 'object') return null;
    var known = ['entry', 'balanced', 'difference', 'lines', 'renames', 'skipped', 'cells',
      'cells_changed', 'tables', 'rows', 'key_totals', 'destination', 'source', 'op',
      'table_stats', 'issues', 'blocking', 'changed_tables', 'blocked', 'changed',
      'written', 'dangling_before', 'dangling_after', 'already_applied', 'stem',
      'members', 'crc_mismatch', 'net_income_cents', 'closing_accounts', 'p_twins'];
    var out = {};
    var any = false;
    Object.keys(p).forEach(function (k) {
      if (known.indexOf(k) < 0 && p[k] !== null && p[k] !== undefined) { out[k] = p[k]; any = true; }
    });
    return any ? out : null;
  }

  function renderOpSpecific(op, kind, p) {
    var base = String(op || kind || '');
    if (/entries/.test(base) || /entries/.test(kind)) return renderEntryPreview(p);
    if (/renumber/.test(base) || /renumber/.test(kind)) return renderRenumberPreview(p);
    if (/yearend|year_end/.test(base) || /yearend/.test(kind)) return renderYearendPreview(p);
    if (/lifecycle|expand|compress|backup/.test(base) || /lifecycle/.test(kind)) return renderLifecyclePreview(p);
    return null;
  }

  function renderEntryPreview(p) {
    var e = p.entry || p;
    var lines = p.lines || e.lines || [];
    var wrap = mk('div', { class: 'panel' });
    wrap.appendChild(mk('div', { class: 'panel-title', text: 'Entry ' + text(e.ref || e.system_id || '') }));
    var balanced = p.balanced !== undefined ? !!p.balanced : (e.balanced !== undefined ? !!e.balanced : null);
    wrap.appendChild(mk('table', { class: 'kv', style: { marginBottom: '6px' } }, mk('tbody', null, [
      mk('tr', null, [mk('th', { text: 'Date' }), mk('td', { class: 'mono', text: text(e.date) })]),
      mk('tr', null, [mk('th', { text: 'Description' }), mk('td', { text: text(e.desc) || '\u2014' })]),
      mk('tr', null, [mk('th', { text: 'Type' }), mk('td', { text: text(e.type_label || e.type) })]),
      mk('tr', null, [mk('th', { text: 'Period' }), mk('td', { text: text(e.period) || '\u2014' })]),
      mk('tr', null, [mk('th', { text: 'Balance' }), mk('td', null, balanced === null
        ? mk('span', { class: 'pill', text: 'unknown' })
        : mk('span', { class: 'pill ' + (balanced ? 'pill-ok' : 'pill-bad'), text: balanced ? 'balanced' : 'difference ' + fmt.num(p.difference !== undefined ? p.difference : e.difference) }))])
    ])));

    var body = mk('tbody');
    lines.forEach(function (l) {
      body.appendChild(mk('tr', null, [
        mk('td', { class: 'mono', text: text(l.acct) }),
        mk('td', { class: 'desc', text: text(l.desc) }),
        mk('td', { class: 'num', text: fmt.num(l.amount) })
      ]));
    });
    if (!lines.length) body.appendChild(mk('tr', null, mk('td', { class: 'empty-cell', colspan: '3', text: 'No lines.' })));
    wrap.appendChild(mk('table', { class: 'grid compact' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { class: 'static', text: 'Account' }),
        mk('th', { class: 'static', text: 'Description' }),
        mk('th', { class: 'num static', text: 'Amount' })
      ])),
      body
    ]));
    return wrap;
  }

  function renderRenumberPreview(p) {
    var wrap = mk('div', { class: 'panel' });
    var renames = p.renames || p.plan || [];
    var cells = p.cells !== undefined ? p.cells : p.cells_changed;
    wrap.appendChild(mk('div', { class: 'btn-row', style: { marginBottom: '6px' } }, [
      mk('span', { class: 'pill pill-info', text: (renames.length || 0) + ' renames' }),
      cells !== undefined ? mk('span', { class: 'pill', text: fmt.int(cells) + ' cells' }) : null,
      (p.skipped && p.skipped.length) ? mk('span', { class: 'pill pill-warn', text: p.skipped.length + ' skipped' }) : null,
      (p.dangling_after && p.dangling_after.length) ? mk('span', { class: 'pill pill-bad', text: p.dangling_after.length + ' dangling refs' }) : null
    ]));

    if (Array.isArray(p.tables) && p.tables.length) {
      var tb = mk('tbody');
      p.tables.forEach(function (t) {
        tb.appendChild(mk('tr', null, [
          mk('td', { class: 'mono', text: text(t.table) }),
          mk('td', { class: 'num', text: fmt.int(t.cells !== undefined ? t.cells : t.before) }),
          mk('td', { class: 'num', text: fmt.int(t.after) }),
          mk('td', { class: 'num', text: text(t.rows) })
        ]));
      });
      wrap.appendChild(mk('table', { class: 'diff' }, [
        mk('caption', { text: 'Cascade by table' }),
        mk('thead', null, mk('tr', null, [
          mk('th', { text: 'Table' }), mk('th', { class: 'num', text: 'Cells' }), mk('th', { class: 'num', text: 'After' }), mk('th', { class: 'num', text: 'Rows' })
        ])),
        tb
      ]));
    }

    if (renames.length) {
      var rb = mk('tbody');
      var cap = Math.min(renames.length, 200);
      for (var i = 0; i < cap; i++) {
        var r = renames[i];
        rb.appendChild(mk('tr', { class: 'ch' }, [
          mk('td', { class: 'mono', text: text(r.from) }),
          mk('td', { text: '\u2192' }),
          mk('td', { class: 'mono', text: text(r.to) })
        ]));
      }
      wrap.appendChild(mk('table', { class: 'diff' }, [
        mk('caption', { text: 'Account number renames' + (renames.length > cap ? ' (first ' + cap + ' of ' + renames.length + ')' : '') }),
        mk('thead', null, mk('tr', null, [mk('th', { text: 'From' }), mk('th', { text: '' }), mk('th', { text: 'To' })])),
        rb
      ]));
    }

    if (p.skipped && p.skipped.length) {
      wrap.appendChild(mk('div', { class: 'small muted', text: 'Skipped (non-canonical): ' + p.skipped.slice(0, 60).join(', ') + (p.skipped.length > 60 ? '\u2026' : '') }));
    }
    var issues = p.issues || p.blocking || [];
    if (issues.length) {
      var ul = mk('ul', { class: 'issues' });
      issues.slice(0, 100).forEach(function (it) {
        ul.appendChild(mk('li', { text: text(it && typeof it === 'object' ? (it.message || it.reason || JSON.stringify(it)) : it) }));
      });
      wrap.appendChild(mk('div', { class: 'panel', style: { marginTop: '6px', background: 'var(--red-bg)', borderColor: '#e8b6b6' } }, [
        mk('div', { class: 'panel-title', style: { color: 'var(--red)' }, text: 'Blocking issues (' + issues.length + ') \u2014 the cascade refuses to run' }),
        ul,
        issues.length > 100 ? mk('div', { class: 'small muted', text: '\u2026 and ' + (issues.length - 100) + ' more' }) : null
      ]));
    }
    if (p.changed_tables) {
      var ct = diffEntries(Array.isArray(p.changed_tables)
        ? p.changed_tables.map(function (x) { return typeof x === 'string' ? x : (x.table || x.name); })
        : null);
      if (ct.length) wrap.appendChild(mk('div', { class: 'small muted', text: 'Tables a commit would touch: ' + ct.map(function (x) { return x.name; }).join(', ') }));
    }
    return wrap;
  }

  function renderYearendPreview(p) {
    var wrap = mk('div', { class: 'panel' });
    var rows = p.rows || [];
    if (rows.length) {
      var tb = mk('tbody');
      rows.forEach(function (r) {
        var d = (Number(r.after) || 0) - (Number(r.before) || 0);
        tb.appendChild(mk('tr', { class: d !== 0 ? 'ch' : '' }, [
          mk('td', { class: 'mono', text: text(r.table || r.name) }),
          mk('td', { class: 'num', text: fmt.int(r.before) }),
          mk('td', { class: 'num', text: fmt.int(r.after) }),
          mk('td', { class: 'num', text: (d > 0 ? '+' : '') + fmt.int(d) })
        ]));
      });
      wrap.appendChild(mk('table', { class: 'diff' }, [
        mk('caption', { text: 'Destination row counts (before \u2192 after)' }),
        mk('thead', null, mk('tr', null, [
          mk('th', { text: 'Table' }), mk('th', { class: 'num', text: 'Source rows' }), mk('th', { class: 'num', text: 'Destination rows' }), mk('th', { class: 'num', text: 'Delta' })
        ])),
        tb
      ]));
    }
    if (p.destination) wrap.appendChild(mk('div', { class: 'small mono', text: 'destination: ' + p.destination }));
    if (p.key_totals) wrap.appendChild(mk('div', { style: { marginTop: '6px' } }, renderGeneric(p.key_totals)));
    if (p.net_income_cents !== undefined) {
      wrap.appendChild(mk('div', { class: 'small', text: 'Net income absorbed into retained earnings: ' + fmt.num(Number(p.net_income_cents) / 100) }));
    }
    if (p.closing_accounts !== undefined) {
      wrap.appendChild(mk('div', { class: 'small muted', text: 'Closing accounts: ' + fmt.int(p.closing_accounts) + (p.p_twins !== undefined ? ' \u00b7 prior-year twins: ' + fmt.int(p.p_twins) : '') }));
    }
    return wrap;
  }

  function renderLifecyclePreview(p) {
    var wrap = mk('div', { class: 'panel' });
    if (p.source || p.destination) {
      wrap.appendChild(mk('table', { class: 'kv', style: { marginBottom: '6px' } }, mk('tbody', null, [
        mk('tr', null, [mk('th', { text: 'Source' }), mk('td', { class: 'mono', text: text(p.source) })]),
        mk('tr', null, [mk('th', { text: 'Destination' }), mk('td', { class: 'mono', text: text(p.destination) })])
      ])));
    }
    var tables = p.tables || [];
    if (Array.isArray(tables) && tables.length) {
      var tb = mk('tbody');
      tables.forEach(function (t) {
        tb.appendChild(mk('tr', null, [
          mk('td', { class: 'mono', text: text(t.table || t.name) }),
          mk('td', { class: 'num', text: fmt.int(t.src_live !== undefined ? t.src_live : t.before) }),
          mk('td', { class: 'num', text: fmt.int(t.dst_live !== undefined ? t.dst_live : t.after) }),
          mk('td', { class: 'num', text: fmt.int(t.src_deleted) }),
          mk('td', { class: 'num', text: fmt.int(t.dst_deleted) })
        ]));
      });
      wrap.appendChild(mk('table', { class: 'diff' }, [
        mk('caption', { text: 'Table row-set comparison' }),
        mk('thead', null, mk('tr', null, [
          mk('th', { text: 'Table' }), mk('th', { class: 'num', text: 'Src live' }), mk('th', { class: 'num', text: 'Dst live' }),
          mk('th', { class: 'num', text: 'Src deleted' }), mk('th', { class: 'num', text: 'Dst deleted' })
        ])),
        tb
      ]));
    }
    return wrap;
  }

  function renderGeneric(obj) {
    if (obj === null || obj === undefined) return mk('div', { class: 'small muted', text: '(no preview payload)' });
    if (typeof obj !== 'object') return mk('div', { class: 'small', text: String(obj) });
    if (Array.isArray(obj)) {
      if (!obj.length) return mk('div', { class: 'small muted', text: '(empty list)' });
      return mk('pre', { class: 'mono small scroll-x', style: { margin: '0', whiteSpace: 'pre-wrap' }, text: JSON.stringify(obj, null, 2) });
    }
    var body = mk('tbody');
    var keys = Object.keys(obj);
    if (!keys.length) return mk('div', { class: 'small muted', text: '(empty)' });
    keys.forEach(function (k) {
      var v = obj[k];
      var cell;
      if (v !== null && typeof v === 'object') {
        cell = mk('td', { class: 'mono', text: JSON.stringify(v) });
      } else {
        cell = mk('td', { class: 'mono', text: v === null || v === undefined ? '' : String(v) });
      }
      body.appendChild(mk('tr', null, [mk('th', { text: k }), cell]));
    });
    return mk('table', { class: 'kv' }, body);
  }

  /* -------------------------------------------------------------- diff view */

  function diffEntries(list) {
    return (list || []).map(function (x) { return typeof x === 'string' ? { name: x } : x; });
  }

  function renderDiff(m) {
    if (!m) return null;
    var onlyA = diffEntries(m.only_a);
    var onlyB = diffEntries(m.only_b);
    var changed = diffEntries(m.changed);
    if (!onlyA.length && !onlyB.length && !changed.length) {
      return mk('div', { class: 'empty', text: 'No file differences between the two manifests.' });
    }
    var wrap = mk('div');
    wrap.appendChild(mk('div', { class: 'diff-summary' }, [
      mk('span', { class: 'pill pill-bad', text: onlyA.length + ' only before' }),
      mk('span', { class: 'pill pill-ok', text: onlyB.length + ' only after' }),
      mk('span', { class: 'pill pill-info', text: changed.length + ' changed' })
    ]));

    function table(caption, rows, cls, cols) {
      var body = mk('tbody');
      var cap = Math.min(rows.length, 300);
      for (var i = 0; i < cap; i++) {
        var r = rows[i];
        var tr = mk('tr', { class: cls });
        if (cols === 'only') {
          /* `only_a`/`only_b` may be bare names, so keep every column present
             (empty when the server reported no size/digest for that side). */
          tr.appendChild(mk('td', { class: 'name', text: text(r.name) }));
          tr.appendChild(mk('td', { class: 'num', text: r.bytes === undefined || r.bytes === null ? '' : fmt.bytes(r.bytes) }));
          tr.appendChild(mk('td', { class: 'sha', text: fmt.sha(r.sha256) }));
        } else {
          tr.appendChild(mk('td', { class: 'name', text: text(r.name) }));
          tr.appendChild(mk('td', { class: 'num', text: fmt.bytes(r.a_bytes !== undefined ? r.a_bytes : r.before_bytes) }));
          tr.appendChild(mk('td', { class: 'num', text: fmt.bytes(r.b_bytes !== undefined ? r.b_bytes : r.after_bytes) }));
          tr.appendChild(mk('td', { class: 'sha', text: fmt.sha(r.a_sha || r.before_sha) }));
          tr.appendChild(mk('td', { class: 'sha', text: fmt.sha(r.b_sha || r.after_sha) }));
        }
        body.appendChild(tr);
      }
      var head = cols === 'only'
        ? mk('tr', null, [mk('th', { text: 'File' }), mk('th', { class: 'num', text: 'Bytes' }), mk('th', { text: 'SHA-256' })])
        : mk('tr', null, [
          mk('th', { text: 'File' }), mk('th', { class: 'num', text: 'Before' }), mk('th', { class: 'num', text: 'After' }),
          mk('th', { text: 'SHA before' }), mk('th', { text: 'SHA after' })
        ]);
      return mk('table', { class: 'diff' }, [
        mk('caption', { text: caption + (rows.length > cap ? ' (first ' + cap + ' of ' + rows.length + ')' : '') }),
        mk('thead', null, head),
        body
      ]);
    }

    if (onlyA.length) wrap.appendChild(table('Only in the before snapshot', onlyA, 'a', 'only'));
    if (onlyB.length) wrap.appendChild(table('Only in the after snapshot', onlyB, 'b', 'only'));
    if (changed.length) wrap.appendChild(table('Changed files', changed, 'ch', 'changed'));
    return wrap;
  }

  /* -------------------------------------------------------- operations view */

  function opForm(fields) {
    var grid = mk('div', { class: 'form-grid' });
    fields.forEach(function (f) {
      grid.appendChild(mk('label', { text: f.label }));
      grid.appendChild(f.input);
    });
    return grid;
  }

  function textInput(value, placeholder, size) {
    return mk('input', { type: 'text', value: value || '', placeholder: placeholder || '', size: size || null });
  }

  function pathDefaults() {
    var base = overviewPath(S.overview);
    var work = workDirHint(base);
    return { base: base, work: work };
  }

  VIEWS.operations = async function (root) {
    root.replaceChildren();
    var pd = pathDefaults();
    root.appendChild(viewHead('Operations Bay', 'Every write is preview-first: inspect the full manifest diff, then commit', [
      mk('button', { class: 'btn', text: 'Refresh state', onclick: function () { refreshOverview(true); VIEWS.operations(root); } })
    ]));

    root.appendChild(mk('div', { class: 'toolbar' }, [
      mk('span', { class: 'small muted', text: 'Engagement: ' }),
      mk('span', { class: 'mono small', text: pd.base || '(unknown)' }),
      mk('span', { class: 'sep' }),
      mk('span', { class: 'small muted', text: 'Work area: ' }),
      mk('span', { class: 'mono small', text: pd.work })
    ]));

    /* --- entry create --- */
    var eRef = textInput('AJE-' + new Date().toISOString().slice(0, 10).replace(/-/g, ''), 'entry ref', '16');
    var eDate = textInput(new Date().toISOString().slice(0, 10).replace(/-/g, ''), 'YYYYMMDD', '10');
    var eDesc = textInput('', 'description', '26');
    var eType = mk('select', { 'aria-label': 'entry type' }, entryTypeOptions('N'));
    var ePeriod = mk('input', { type: 'number', min: '1', max: '12', value: '12' });
    var eFrom = textInput('', 'ref being replaced (optional)', '18');
    var eLines = [{ acct: '', amount: '' }, { acct: '', amount: '' }];
    var eBal = mk('span', { class: 'pill', text: 'sum 0.00' });

    var linesTable = mk('table', { class: 'lines-editor' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { text: 'Account' }), mk('th', { text: 'Amount (signed)' }), mk('th', { text: '' })
      ]))
    ]);
    var linesBody = mk('tbody');
    linesTable.appendChild(linesBody);
    var linesFoot = mk('tfoot');
    linesTable.appendChild(linesFoot);

    function recomputeLines() {
      var sum = 0;
      var filled = 0;
      eLines.forEach(function (l) {
        var v = fmt.amount(l.amount);
        if (isFinite(v)) { sum += v; filled++; }
      });
      eBal.textContent = 'sum ' + sum.toFixed(2);
      eBal.className = 'pill ' + (Math.abs(sum) < 0.005 && filled >= 2 ? 'pill-ok' : 'pill-warn');
      linesFoot.replaceChildren(mk('tr', null, [
        mk('td', { text: 'Difference' }),
        mk('td', { class: 'amt', text: sum.toFixed(2) }),
        mk('td', null, eBal)
      ]));
    }

    function renderLines() {
      linesBody.replaceChildren();
      eLines.forEach(function (l, i) {
        var a = mk('input', { type: 'text', value: l.acct, placeholder: '1000' });
        a.addEventListener('input', function () { l.acct = a.value; });
        var m = mk('input', { type: 'text', value: l.amount, placeholder: '0.00', inputmode: 'decimal' });
        m.addEventListener('input', function () { l.amount = m.value; recomputeLines(); });
        linesBody.appendChild(mk('tr', null, [
          mk('td', null, a),
          mk('td', { class: 'amt' }, m),
          mk('td', { style: { width: '26px' } }, mk('button', {
            class: 'btn btn-mini', text: '\u00d7', title: 'Remove line',
            onclick: function () {
              if (eLines.length <= 2) { toast('An entry needs at least two lines', 'err', 'input'); return; }
              eLines.splice(i, 1);
              renderLines();
              recomputeLines();
            }
          }))
        ]));
      });
      recomputeLines();
    }
    renderLines();

    var entryForm = mk('div', null, [
      opForm([
        { label: 'Ref', input: eRef },
        { label: 'Date', input: eDate },
        { label: 'Description', input: eDesc },
        { label: 'Type', input: eType },
        { label: 'Period', input: ePeriod },
        { label: 'Replaces', input: eFrom }
      ]),
      linesTable,
      mk('div', { class: 'btn-row' }, mk('button', {
        class: 'btn btn-mini', text: '+ Add line',
        onclick: function () { eLines.push({ acct: '', amount: '' }); renderLines(); }
      }))
    ]);

    var entryCard = opCard({
      kind: 'entries',
      title: 'Create / replace adjusting entry',
      blurb: 'AM is never touched; GL rows are written and BL is recalculated per the F35 laws.',
      form: entryForm,
      body: function () {
        var lines = eLines.map(function (l) { return { acct: String(l.acct).trim(), amount: fmt.amount(l.amount) }; })
          .filter(function (l) { return l.acct && isFinite(l.amount); });
        if (lines.length < 2) throw new Error('Provide at least two complete lines (account + amount).');
        var sum = lines.reduce(function (s, l) { return s + l.amount; }, 0);
        if (Math.abs(sum) >= 0.005) throw new Error('Entry does not balance (difference ' + sum.toFixed(2) + ').');
        var ref = eRef.value.trim();
        if (!ref) throw new Error('An entry reference is required.');
        if (!/^\d{8}$/.test(eDate.value.trim())) throw new Error('Date must be YYYYMMDD.');
        var spec = {
          ref: ref,
          date: eDate.value.trim(),
          desc: eDesc.value.trim(),
          type: eType.value,
          period: Number(ePeriod.value) || 12,
          lines: lines
        };
        var from = eFrom.value.trim();
        if (from) spec.from_ref = from;
        return spec;
      }
    });

    /* --- renumber --- */
    /* Delta is the CLI's primary form (`dialog renumber --delta N`) and needs no
       other input, so it is the default; pairs mode is one select away. */
    var rMode = mk('select', { 'aria-label': 'renumber mode' }, [
      mk('option', { value: 'delta', text: 'Uniform delta shift' }),
      mk('option', { value: 'pairs', text: 'Explicit pairs (from = to)' })
    ]);
    var rDelta = mk('input', { type: 'number', value: '100', 'aria-label': 'delta' });
    var rPairs = mk('textarea', { placeholder: '1001=1099\n1002=1100', rows: '4' });
    rPairs.value = '';
    var rDeltaRow = mk('div', null, rDelta);
    var rPairsRow = mk('div', { style: { marginTop: '6px' } }, [
      mk('div', { class: 'small muted', style: { marginBottom: '3px' }, text: 'One rename per line: FROM=TO' }),
      rPairs
    ]);
    function syncRenumberMode() {
      var isDelta = rMode.value === 'delta';
      rDeltaRow.style.display = isDelta ? '' : 'none';
      rPairsRow.style.display = isDelta ? 'none' : '';
    }
    rMode.addEventListener('change', syncRenumberMode);
    syncRenumberMode();

    var renumberForm = mk('div', null, [
      opForm([
        { label: 'Mode', input: rMode },
        { label: 'Delta', input: rDeltaRow }
      ]),
      rPairsRow
    ]);

    var renumberCard = opCard({
      kind: 'renumber',
      title: 'Renumber accounts (cascade)',
      blurb: 'Rewrites AM, BL and GL together; tombstones are preserved. Preview lists every rewritten cell.',
      form: renumberForm,
      body: function () {
        if (rMode.value === 'delta') {
          var d = Number(rDelta.value);
          if (!isFinite(d) || d === 0) throw new Error('Delta must be a non-zero number.');
          return { delta: d };
        }
        var plan = [];
        rPairs.value.split(/\r?\n/).forEach(function (line) {
          var s = line.trim();
          if (!s) return;
          var m = s.split('=');
          if (m.length !== 2 || !m[0].trim() || !m[1].trim()) throw new Error('Bad rename line: ' + s);
          plan.push({ from: m[0].trim(), to: m[1].trim() });
        });
        if (!plan.length) throw new Error('Provide at least one FROM=TO rename.');
        return { plan: plan };
      }
    });

    /* --- year end --- */
    var yDest = textInput('', 'destination engagement dir (blank = new copy)', '34');
    var yRe = textInput('', 'retained earnings account', '10');
    var yYear = mk('input', { type: 'number', value: '1', min: '0', max: '9' });
    var yearendForm = opForm([
      { label: 'Destination', input: yDest },
      { label: 'Retained earnings', input: yRe },
      { label: 'Closing year', input: yYear }
    ]);
    var yearendCard = opCard({
      kind: 'yearend',
      title: 'Year-end close',
      blurb: 'Builds a NEW engagement copy (year shift +1, net income into retained earnings). Never closes in place without a backup.',
      form: yearendForm,
      body: function () {
        var b = {};
        if (yDest.value.trim()) b.dest = yDest.value.trim();
        if (yRe.value.trim()) b.retained_earning = yRe.value.trim();
        b.year = Number(yYear.value) || 1;
        return b;
      }
    });

    /* --- lifecycle: expand / compress / backup --- */
    function lifecycleCard(opName, title, blurb) {
      var src = textInput(pd.base, 'engagement dir', '34');
      var dest = textInput('', 'destination (blank = server default)', '34');
      var form = opForm([
        { label: 'Source', input: src },
        { label: 'Destination', input: dest }
      ]);
      return opCard({
        kind: 'lifecycle/' + opName,
        title: title,
        blurb: blurb,
        form: form,
        body: function () {
          var b = { path: src.value.trim() };
          if (!b.path) throw new Error('A source path is required.');
          if (dest.value.trim()) b.dest = dest.value.trim();
          return b;
        }
      });
    }

    root.appendChild(mk('div', { class: 'opcards' }, [
      entryCard,
      renumberCard,
      yearendCard,
      lifecycleCard('expand', 'Expand package', 'Unpacks .ac_ into a working engagement directory set.'),
      lifecycleCard('compress', 'Compress package', 'Repacks a working directory into the .ac_ container.'),
      lifecycleCard('backup', 'Backup snapshot', 'Copies the engagement into the work area with a manifest record.')
    ]));

    root.appendChild(mk('h2', { class: 'section', text: 'Session commits' }));
    auditHost = mk('div', { class: 'panel' });
    root.appendChild(auditHost);
    renderAudit();
  };

  /* --------------------------------------------------------------- manifest */

  async function loadManifestPath(p) {
    var data = await getJSON('/api/manifest?path=' + encodeURIComponent(p));
    S.manifest.files = data.files || [];
    S.manifest.total = data.total_bytes !== undefined ? data.total_bytes : data.files.reduce(function (s, f) { return s + (Number(f.bytes) || 0); }, 0);
    S.manifest.path = p;
    return data;
  }

  VIEWS.manifest = async function (root) {
    root.replaceChildren();
    root.appendChild(viewHead('Manifest', 'Recursive file inventory (name, bytes, SHA-256) with snapshot comparison'));

    var pathIn = mk('input', { type: 'text', value: S.manifest.path || overviewPath(S.overview), size: '46', 'aria-label': 'manifest path', placeholder: 'engagement directory path' });
    var loadBtn = mk('button', { class: 'btn btn-primary', text: 'Load' });
    loadBtn.addEventListener('click', function () {
      S.manifest.path = pathIn.value.trim();
      VIEWS.manifest(root);
    });
    pathIn.addEventListener('keydown', function (e) { if (e.key === 'Enter') loadBtn.click(); });

    var aIn = mk('input', { type: 'text', value: S.manifest.a, size: '30', 'aria-label': 'snapshot A', placeholder: 'before path' });
    var bIn = mk('input', { type: 'text', value: S.manifest.b, size: '30', 'aria-label': 'snapshot B', placeholder: 'after path' });
    var cmpBtn = mk('button', { class: 'btn', text: 'Compare' });
    var diffHost = mk('div');

    cmpBtn.addEventListener('click', async function () {
      var a = aIn.value.trim() || S.manifest.path;
      var b = bIn.value.trim();
      if (!a || !b) { toast('Both paths are required to compare', 'err', 'input'); return; }
      S.manifest.a = a; S.manifest.b = b;
      diffHost.replaceChildren(mk('div', { class: 'loading', text: 'Comparing manifests\u2026' }));
      try {
        var data = await postJSON('/api/manifest/diff', { a: a, b: b });
        S.manifest.diff = data;
        var view = renderDiff(data.diff || data);
        diffHost.replaceChildren(view || mk('div', { class: 'empty', text: 'No difference reported.' }));
      } catch (e) {
        S.manifest.diff = null;
        diffHost.replaceChildren(errorPane(e));
        toastErr(e);
      }
    });

    root.appendChild(mk('div', { class: 'toolbar' }, [
      mk('label', { class: 'field' }, [mk('span', { class: 'lbl', text: 'Path' }), pathIn]),
      loadBtn,
      mk('span', { class: 'sep' }),
      mk('span', { class: 'small muted', text: 'Compare snapshots:' }),
      aIn, mk('span', { class: 'small muted', text: '\u2194' }), bIn, cmpBtn
    ]));

    var host = mk('div');
    root.appendChild(host);
    root.appendChild(diffHost);

    if (!S.manifest.path) {
      host.appendChild(mk('div', { class: 'empty', text: 'Enter a directory path to inventory it (defaults to the open engagement).' }));
      return;
    }
    host.appendChild(mk('div', { class: 'loading', text: 'Inventorying ' + S.manifest.path + '\u2026' }));
    try {
      await loadManifestPath(S.manifest.path);
    } catch (e) {
      host.replaceChildren(errorPane(e, function () { VIEWS.manifest(root); }));
      toastErr(e);
      return;
    }
    var files = S.manifest.files.slice().sort(function (x, y) { return String(x.name).localeCompare(String(y.name)); });
    var body = mk('tbody');
    files.slice(0, 2000).forEach(function (f) {
      body.appendChild(mk('tr', null, [
        mk('td', { class: 'mono small', text: text(f.name), title: text(f.name) }),
        mk('td', { class: 'num', text: fmt.int(f.bytes) }),
        mk('td', { class: 'sha mono', text: fmt.sha(f.sha256), title: text(f.sha256) })
      ]));
    });
    if (!files.length) body.appendChild(mk('tr', null, mk('td', { class: 'empty-cell', colspan: '3', text: 'No files found at this path.' })));
    host.replaceChildren(mk('div', { class: 'grid-wrap' }, mk('table', { class: 'grid compact' }, [
      mk('thead', null, mk('tr', null, [
        mk('th', { class: 'static', text: 'File' }),
        mk('th', { class: 'num static', text: 'Bytes' }),
        mk('th', { class: 'static', text: 'SHA-256 (prefix)' })
      ])),
      body
    ])));
    host.appendChild(mk('div', { class: 'small muted', style: { marginTop: '6px' }, text:
      fmt.int(files.length) + ' files \u00b7 ' + fmt.bytes(S.manifest.total) + ' total' +
      (files.length > 2000 ? ' \u00b7 table capped at 2000 rows' : '') }));
  };

  /* ------------------------------------------------------------------ boot */

  function start() {
    buildShell();
    window.addEventListener('hashchange', renderRoute);
    refreshOverview(false).then(function () {
      if (!location.hash) {
        location.replace('#/dashboard');
        renderRoute();
      } else {
        renderRoute();
      }
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', start);
  } else {
    start();
  }
})();
