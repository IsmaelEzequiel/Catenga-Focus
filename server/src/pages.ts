export const escapeHtml = (s: string): string =>
  s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");

/** Copy of `branding/mark.svg` (the calango on the sun), inlined so the pages need no static files. */
const MARK = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120" width="36" height="36" aria-hidden="true"><circle cx="60" cy="60" r="50" fill="#E9A23B"/><g transform="rotate(-35 60 60)" fill="#1E2B22"><ellipse cx="60" cy="27" rx="8" ry="10"/><ellipse cx="60" cy="51" rx="9.5" ry="17"/><path d="M54.5 64C54 84 70 94 68 114C74 94 66 82 65.5 64Z"/><path d="M53 41L41 35L39 28M67 41L79 35L81 28M53 61L41 68L39 76M67 61L79 68L81 76" fill="none" stroke="#1E2B22" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/><circle cx="55.5" cy="23" r="1.8" fill="#E9A23B"/><circle cx="64.5" cy="23" r="1.8" fill="#E9A23B"/></g></svg>`;
const FAVICON = `data:image/svg+xml,${encodeURIComponent(MARK)}`;

const STYLE = `
  *{box-sizing:border-box}
  body{margin:0;background:#F5EFE3;color:#1E2B22;font-family:Inter,system-ui,sans-serif;line-height:1.5}
  main{max-width:26rem;margin:0 auto;padding:2rem 1rem}
  .brand{display:flex;align-items:center;gap:.6rem;font-size:1.5rem;margin:0 0 1.5rem}
  .brand svg{flex:none}
  .brand b{font-weight:700}.brand span{font-weight:400}
  .card{background:#FFFCF5;border:1px solid #E3D9C6;border-radius:14px;padding:1.5rem}
  h1{font-size:1.25rem;margin:0 0 1rem}
  label{display:block;font-size:.875rem;margin:0 0 1rem}
  input[type=text],input[type=email]{display:block;width:100%;margin-top:.25rem;padding:.6rem .75rem;font:inherit;border:1px solid #E3D9C6;border-radius:8px;background:#fff;color:inherit}
  button{font:inherit;font-weight:600;background:#E9A23B;color:#1E2B22;border:0;border-radius:8px;padding:.6rem 1.1rem;cursor:pointer}
  a{color:#1E2B22}
  .error{color:#C8553D;margin:0 0 1rem}
  .muted{color:#5B6459;font-size:.875rem}
`;

export function layout(title: string, content: string): string {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)} · Catenga Focus</title>
<link rel="icon" href="${FAVICON}">
<style>${STYLE}</style>
</head>
<body>
<main>
<p class="brand">${MARK}<span><b>Catenga</b> Focus</span></p>
<div class="card">
${content}
</div>
</main>
</body>
</html>
`;
}

export type AppParams = { appPort: number; codeChallenge: string } | null;

const hiddenAppParams = (app: AppParams): string =>
  app
    ? `<input type="hidden" name="app_port" value="${app.appPort}">
<input type="hidden" name="code_challenge" value="${escapeHtml(app.codeChallenge)}">`
    : "";

const appQuery = (app: AppParams): string =>
  app ? `?app_port=${app.appPort}&amp;code_challenge=${escapeHtml(app.codeChallenge)}` : "";

export const homePage = (user: { name: string; email: string } | null): string =>
  layout(
    "Home",
    user
      ? `<h1>Signed in as ${escapeHtml(user.name)} (${escapeHtml(user.email)})</h1>
<form method="post" action="/logout"><button type="submit">Sign out</button></form>`
      : `<h1>Welcome</h1>
<p><a href="/login">Sign in</a> or <a href="/register">create an account</a>.</p>
<p class="muted">An account is optional: Catenga Focus works fully without one.</p>`,
  );

export const registerPage = (
  app: AppParams,
  values: { name: string; email: string } = { name: "", email: "" },
  error?: string,
): string =>
  layout(
    "Create account",
    `<h1>Create an account</h1>
${error ? `<p class="error">${escapeHtml(error)}</p>` : ""}
<form method="post" action="/register">
${hiddenAppParams(app)}
<label>Name<input type="text" name="name" value="${escapeHtml(values.name)}" maxlength="100" required></label>
<label>Email<input type="email" name="email" value="${escapeHtml(values.email)}" maxlength="254" required></label>
<button type="submit">Create account</button>
</form>
<p class="muted">Already have one? <a href="/login${appQuery(app)}">Sign in</a></p>`,
  );

export const loginPage = (app: AppParams, email = "", error?: string): string =>
  layout(
    "Sign in",
    `<h1>Sign in</h1>
${error ? `<p class="error">${escapeHtml(error)}</p>` : ""}
<form method="post" action="/login">
${hiddenAppParams(app)}
<label>Email<input type="email" name="email" value="${escapeHtml(email)}" maxlength="254" required></label>
<button type="submit">Email me a sign-in link</button>
</form>
<p class="muted">No account yet? <a href="/register${appQuery(app)}">Create an account</a></p>`,
  );

export const checkEmailPage = (email: string, hedged: boolean): string =>
  layout(
    "Check your email",
    hedged
      ? `<h1>Check your email</h1>
<p>If an account exists for ${escapeHtml(email)}, we sent a sign-in link. It works once and expires in 15 minutes.</p>`
      : `<h1>Check your email</h1>
<p>We sent a sign-in link to ${escapeHtml(email)}. It works once and expires in 15 minutes.</p>`,
  );

export const verifyPage = (token: string): string =>
  layout(
    "Sign in",
    `<h1>Sign in to Catenga Focus</h1>
<form method="post" action="/auth/verify">
<input type="hidden" name="token" value="${escapeHtml(token)}">
<button type="submit">Sign in</button>
</form>
<p class="muted">If you opened this link from the app, open it on the computer where Catenga Focus is running.</p>`,
  );

export const expiredPage = (): string =>
  layout(
    "Link expired",
    `<h1>Link expired</h1>
<p>This link has expired or was already used.</p>
<p><a href="/login">Request a new sign-in link</a></p>`,
  );

export const errorPage = (title: string, message: string): string =>
  layout(title, `<h1>${escapeHtml(title)}</h1>\n<p>${escapeHtml(message)}</p>`);
