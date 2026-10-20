import glyph from "@/assets/bongbetic-glyph.png";
import { t } from "@/lib/i18n";

/** Footer credit row. The glyph is decorative: the text next to it names the brand. */
export function Credit() {
  // Used as a CSS mask so the glyph takes the current text colour in both themes.
  const url = `url(${glyph})`;
  return (
    <footer className="ph-credit">
      <span
        className="ph-credit__glyph"
        aria-hidden="true"
        data-testid="credit-glyph"
        style={{ maskImage: url, WebkitMaskImage: url }}
      />
      <span>{t("app.credit")}</span>
    </footer>
  );
}
