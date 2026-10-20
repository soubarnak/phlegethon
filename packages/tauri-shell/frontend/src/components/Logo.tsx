import { LOGO_CIRCLES, LOGO_COLORS, LOGO_LETTERS_PATH, LOGO_VIEWBOX } from "./logoPaths";

/** Phlegethon wordmark. Letters follow the theme via currentColor; the "o" is three blended ember circles. */
export function Logo({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      role="img"
      aria-label="Phlegethon"
      viewBox={`0 0 ${LOGO_VIEWBOX.width} ${LOGO_VIEWBOX.height}`}
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d={LOGO_LETTERS_PATH} fill="currentColor" />
      <g style={{ isolation: "isolate" }}>
        {LOGO_CIRCLES.map(([cx, cy, r], i) => (
          <circle
            key={i}
            data-testid="logo-circle"
            cx={cx}
            cy={cy}
            r={r}
            fill={LOGO_COLORS[i]}
            style={{ mixBlendMode: "multiply" }}
          />
        ))}
      </g>
    </svg>
  );
}
