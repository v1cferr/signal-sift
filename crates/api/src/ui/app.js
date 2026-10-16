// SignalSift local web UI. Plain JavaScript, no build step, no dependencies.
//
// Everything shown here comes from third parties (titles, summaries,
// transcripts) or from a model, so it is inserted with textContent only,
// never as HTML, and links are created only for http(s) URLs.
//
// The interface text is Brazilian Portuguese by the owner's choice (ADR 0010);
// every visible string lives in T below, so the code stays in en-US.

"use strict";

const TOKEN_KEY = "signalsift.token";
const DAYS_KEY = "signalsift.days";
const LOCALE = "pt-BR";

// ---------- Interface text (pt-BR) ----------

const T = {
  loading: "carregando…",
  feedError: (m) => `não foi possível carregar os destaques: ${m}`,
  feedEmpty: "Nada analisado nesta janela ainda. Os itens aparecem aqui assim que a transcrição ou o texto recebe uma nota.",
  itemError: (id, m) => `não foi possível carregar o item ${id}: ${m}`,
  scoreTitle: "relevância estimada (0 a 100)",
  partial: (pct) => `parcial ${pct}%`,
  partialTitle: "só parte deste item foi transcrita",
  why: "Por quê: ",
  segments: "trechos",
  segmentsTitle: (n) => `Trechos (${n})`,
  more: "mais como este",
  less: "não me interessa",
  notAnalysed: "Ainda não analisado.",
  applications: "Aplicações",
  mentions: "citados:",
  back: "← voltar aos destaques",
  text: "texto",
  part: (n) => `parte ${n}`,
  likedToast: "anotado: mais conteúdo assim",
  dislikedToast: "anotado: menos conteúdo assim (o item continua disponível na lista)",
  feedbackFailed: (m) => `não foi possível registrar: ${m}`,
  profileToast: (p) => `perfil: ${p}`,
  profileFailed: (m) => `não foi possível trocar o perfil: ${m}`,
  tokenForgotten: "token esquecido",
  tokenSet: "token ✓",
  token: "token",
  unreachable: "daemon fora do ar",
  status: (s, rss) =>
    `daemon ${s.version} · ${s.database.items} itens · ` +
    `${s.database.jobs_queued} na fila, ${s.database.jobs_running} em execução · ${rss} de RAM`,
};

// Labels for API values. Unknown values are shown as they come.
const LABELS = {
  video: "vídeo",
  article: "artigo",
  podcast_episode: "episódio de podcast",
  manual_subtitles: "legendas",
  auto_subtitles: "legendas automáticas",
  asr: "transcrição local",
  complete: "completa",
  partial: "parcial",
  in_progress: "em andamento",
  failed: "falhou",
  balanced: "equilibrado",
  gaming: "jogos",
  eco: "economia",
  pt: "português",
  en: "inglês",
};
const label = (v) => (v in LABELS ? LABELS[v] : v);

// API error codes in Portuguese; the API's own messages are in English.
const ERRORS = {
  unauthorized: "token ausente ou inválido",
  not_found: "não encontrado",
  bad_request: "pedido inválido",
  payload_too_large: "pedido grande demais",
  internal: "erro interno do daemon (veja os logs)",
  network: "daemon fora do ar",
};
const errorText = (e) => ERRORS[e.code] || e.message;

// ---------- DOM helpers ----------

/** Creates an element. Strings among `children` become text nodes. */
function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = value;
    else if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else el.setAttribute(key, value === true ? "" : String(value));
  }
  for (const child of children.flat()) {
    if (child === undefined || child === null || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

/** Only http(s) URLs become links; anything else is dropped. */
function safeUrl(url) {
  try {
    const u = new URL(url);
    return u.protocol === "https:" || u.protocol === "http:" ? u.href : null;
  } catch {
    return null;
  }
}

function link(url, text, cls) {
  const href = safeUrl(url);
  if (!href) return h("span", { class: cls }, text);
  return h("a", { href, class: cls, target: "_blank", rel: "noopener noreferrer" }, text);
}

function scoreBadge(score) {
  if (score === null || score === undefined) return null;
  const band = score >= 70 ? "s-hi" : score >= 40 ? "s-mid" : "s-lo";
  return h("span", { class: `score ${band}`, title: T.scoreTitle }, String(score));
}

function toast(message, isError) {
  const el = h("div", { class: isError ? "toast error" : "toast", role: "status" }, message);
  document.body.append(el);
  setTimeout(() => el.remove(), 3500);
}

const view = () => document.getElementById("view");

// Publication dates from platforms are often day-only, stored as midnight
// UTC; formatting them in local time would show the previous day west of
// UTC. Shown in UTC, they read as the date the platform reported.
function fmtDate(iso) {
  if (!iso) return null;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d.toLocaleDateString(LOCALE, { timeZone: "UTC" });
}

/** A metadata row: plain values become separate items, so they get spacing. */
function meta(...parts) {
  return h("div", { class: "meta" },
    parts.flat().filter((p) => p !== null && p !== undefined && p !== false && p !== "")
      .map((p) => (p instanceof Node ? p : h("span", null, p))));
}

const percent = (ratio) => Math.round((ratio || 0) * 100);

// ---------- API ----------

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function api(path, options = {}) {
  const headers = { Accept: "application/json" };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  if (options.auth) {
    const token = localStorage.getItem(TOKEN_KEY);
    if (!token) throw new ApiError(401, "unauthorized", "token required");
    headers.Authorization = `Bearer ${token}`;
  }
  let res;
  try {
    res = await fetch(path, {
      method: options.method || "GET",
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
  } catch (e) {
    throw new ApiError(0, "network", e.message);
  }
  const data = await res.json().catch(() => null);
  if (!res.ok) {
    const err = (data && data.error) || {};
    throw new ApiError(res.status, err.code || "error", err.message || res.statusText);
  }
  return data;
}

/** Runs a write; asks for the token once when the daemon rejects it. */
async function withToken(write) {
  try {
    return await write();
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) {
      if (await askToken()) return write();
      return null;
    }
    throw e;
  }
}

function askToken() {
  const dialog = document.getElementById("token-dialog");
  const input = document.getElementById("token-input");
  input.value = "";
  dialog.showModal();
  return new Promise((resolve) => {
    dialog.addEventListener(
      "close",
      () => {
        if (dialog.returnValue === "save" && input.value.trim()) {
          localStorage.setItem(TOKEN_KEY, input.value.trim());
          resolve(true);
        } else {
          if (dialog.returnValue === "forget") {
            localStorage.removeItem(TOKEN_KEY);
            toast(T.tokenForgotten);
          }
          resolve(false);
        }
        updateTokenButton();
      },
      { once: true },
    );
  });
}

function updateTokenButton() {
  const btn = document.getElementById("token");
  const has = Boolean(localStorage.getItem(TOKEN_KEY));
  btn.textContent = has ? T.tokenSet : T.token;
  btn.classList.toggle("on", has);
}

// ---------- Feedback ----------

async function sendFeedback(item, verdict, card) {
  const body = { item_id: item.item_id, verdict, client: "web" };
  if (item.moment && item.moment.segment_id) body.segment_id = item.moment.segment_id;
  try {
    const done = await withToken(() => api("/v1/feedback", { method: "POST", auth: true, body }));
    if (!done) return;
    if (verdict === "interested") {
      card.querySelector(".like").classList.add("on");
      toast(T.likedToast);
    } else {
      // The feed leaves out items marked not interested; mirror that now.
      card.remove();
      toast(T.dislikedToast);
      if (!view().querySelector(".card")) renderFeed();
    }
  } catch (e) {
    toast(T.feedbackFailed(errorText(e)), true);
  }
}

// ---------- Views ----------

async function renderFeed() {
  const days = localStorage.getItem(DAYS_KEY) || "14";
  document.getElementById("days").value = days;
  view().replaceChildren(h("p", { class: "empty" }, T.loading));
  let feed;
  try {
    feed = await api(`/v1/feed?limit=20&days=${encodeURIComponent(days)}`);
  } catch (e) {
    view().replaceChildren(h("p", { class: "empty" }, T.feedError(errorText(e))));
    return;
  }
  if (!feed.highlights.length) {
    view().replaceChildren(h("p", { class: "empty" }, T.feedEmpty));
    return;
  }
  view().replaceChildren(...feed.highlights.map(highlightCard));
}

function highlightCard(item) {
  const card = h("article", { class: "card" });
  const moment = item.moment;
  card.append(
    meta(
      scoreBadge(item.relevance),
      item.source,
      fmtDate(item.published_at),
      label(item.kind),
      item.processing && item.processing.partial
        ? h("span", { class: "chip partial", title: T.partialTitle }, T.partial(percent(item.processing.coverage)))
        : null),
    h("h2", null, link(item.url, item.title)),
    h("div", { class: "meta" }, (item.topics || []).slice(0, 5).map((t) => h("span", { class: "chip" }, t))),
    h("p", { class: "summary" }, item.summary),
    h("p", { class: "why" }, h("b", null, T.why), item.why),
    h("div", { class: "actions" },
      moment && moment.link ? link(moment.link, `▶ ${moment.start}`, "moment") : null,
      h("a", { href: `#/item/${item.item_id}` }, T.segments),
      h("span", { class: "spacer" }),
      h("button", { class: "ghost like", type: "button", title: T.more, "aria-label": T.more,
                    onclick: () => sendFeedback(item, "interested", card) }, "👍"),
      h("button", { class: "ghost", type: "button", title: T.less, "aria-label": T.less,
                    onclick: () => sendFeedback(item, "not_interested", card) }, "👎")),
  );
  return card;
}

async function renderItem(id) {
  view().replaceChildren(h("p", { class: "empty" }, T.loading));
  let item, segs;
  try {
    [item, segs] = await Promise.all([api(`/v1/items/${id}`), api(`/v1/items/${id}/segments`)]);
  } catch (e) {
    view().replaceChildren(h("p", { class: "empty" }, T.itemError(id, errorText(e))));
    return;
  }
  const a = item.analysis;
  const t = item.transcript;
  const header = h("article", { class: "card" },
    meta(
      scoreBadge(item.relevance), item.author, fmtDate(item.published_at),
      t ? `${label(t.origin)} · ${label(t.language)} · ${label(t.status)}` : null,
      t && t.partial ? h("span", { class: "chip partial" }, T.partial(percent(t.coverage))) : null),
    h("h2", null, link(item.url, item.title)),
    a ? h("p", { class: "summary" }, a.summary) : h("p", { class: "muted" }, T.notAnalysed),
    a && a.applications.length
      ? h("div", null, h("b", { class: "muted" }, T.applications), h("ul", { class: "list" }, a.applications.map((x) => h("li", null, x))))
      : null,
    a && a.mentions.length
      ? h("div", { class: "meta" }, T.mentions, a.mentions.map((m) => h("span", { class: "chip" }, m)))
      : null,
    a ? h("p", { class: "muted" }, `${a.model} · ${a.prompt_version}`) : null,
    h("p", null, h("a", { href: "#/" }, T.back)),
  );

  const list = h("ol", { class: "segments" });
  for (const s of segs.segments) {
    const sa = s.analysis;
    list.append(h("li", { class: "segment" },
      h("div", { class: "head" },
        s.link ? link(s.link, s.start || "▶", "moment") : h("span", { class: "moment" }, s.start || T.part(s.seq + 1)),
        scoreBadge(sa ? sa.relevance : null),
        s.title ? h("b", null, s.title) : null,
        sa ? sa.topics.slice(0, 4).map((x) => h("span", { class: "chip" }, x)) : null),
      sa ? h("div", { class: "bar-score" }, h("span", null)) : null,
      sa ? h("p", { class: "summary" }, sa.summary) : null,
      sa ? h("p", { class: "why" }, h("b", null, T.why), sa.rationale) : null,
      h("details", null, h("summary", null, T.text), h("p", null, s.text)),
    ));
    // Width set through the CSSOM: allowed by the CSP, unlike inline styles.
    if (sa) list.lastChild.querySelector(".bar-score > span").style.width = `${sa.relevance}%`;
  }
  view().replaceChildren(header, h("article", { class: "card" }, h("h2", null, T.segmentsTitle(segs.segments.length)), list));
}

// ---------- Status and profile ----------

async function refreshStatus() {
  const el = document.getElementById("status");
  try {
    const s = await api("/v1/status");
    const rss = s.process
      ? `${(s.process.rss_kib / 1024).toLocaleString(LOCALE, { maximumFractionDigits: 1 })} MiB`
      : "n/d";
    el.textContent = T.status(s, rss);
    document.getElementById("profile").value = s.profile;
  } catch {
    el.textContent = T.unreachable;
  }
}

async function changeProfile(event) {
  const profile = event.target.value;
  try {
    const res = await withToken(() => api("/v1/profile", { method: "PUT", auth: true, body: { profile } }));
    if (res) toast(T.profileToast(label(res.profile)));
  } catch (e) {
    toast(T.profileFailed(errorText(e)), true);
  }
  refreshStatus();
}

// ---------- Router ----------

function route() {
  const m = location.hash.match(/^#\/item\/(\d+)$/);
  if (m) renderItem(m[1]);
  else renderFeed();
  window.scrollTo(0, 0);
}

document.addEventListener("DOMContentLoaded", () => {
  document.getElementById("profile").addEventListener("change", changeProfile);
  document.getElementById("days").addEventListener("change", (e) => {
    localStorage.setItem(DAYS_KEY, e.target.value);
    renderFeed();
  });
  document.getElementById("token").addEventListener("click", () => askToken());
  window.addEventListener("hashchange", route);
  updateTokenButton();
  refreshStatus();
  // Light polling, and only while the tab is visible.
  setInterval(() => {
    if (document.visibilityState === "visible") refreshStatus();
  }, 60000);
  route();
});
