// Sets data-theme before first paint so a dark theme never flashes light (UI-11).
// Kept in step with readStored/systemTheme in src/state/useTheme.ts. A classic script
// served from 'self' satisfies the CSP, which forbids inline scripts.
(function () {
  var theme = null;
  try {
    var raw = window.localStorage.getItem("phlegethon-theme");
    if (raw === "g10" || raw === "light") theme = "g10";
    else if (raw === "g100" || raw === "dark") theme = "g100";
  } catch (e) {
    /* no storage */
  }
  if (!theme) {
    try {
      theme = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches ? "g100" : "g10";
    } catch (e) {
      theme = "g10";
    }
  }
  document.documentElement.setAttribute("data-theme", theme);
})();
