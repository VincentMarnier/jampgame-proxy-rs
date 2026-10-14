// Aurebesh divider glyphs — decorative homage, not a font.
//
// Reference: public-domain Aurebesh chart (Wikimedia Commons,
// "Star-Wars-aurebesh-alphabet-chart-vert.svg", PD-self by AnonMoos).
// Letterforms below are original simplified stroke drawings in this site's
// icon style (24-grid, round joins), checked against that chart — close
// enough for fans to decode, loose enough to stay quiet at 19px.
//
// Transliteration used in the dividers ("JAMPGAME PROXY RS"):
//   J=JENTH A=AUREK M=MERN P=PETH G=GREK E=ESK O=OSK R=RESH X=XESH Y=YIRT S=SENTH
// Purely decorative: rendered as bare SVG paths with no <title>, inside a
// divider that is already aria-hidden, so screen readers skip it.

const GLYPHS = {
  // Aurek: two stacked arms, each flicking down-right.
  A: "M4.5 5.5L4.5 8.5L12 8.5L19.5 12.3M4.5 12.5L4.5 15.5L12 15.5L19.5 19.3",
  // Esk: open V fused to a full-height right stem.
  E: "M4.5 6L11 18L18 18M18 6L18 18",
  // Grek: left stem + top bar with step notch, diagonal brace to the foot.
  G: "M5 6L5 18L18 18M5 6L19 6L10.5 18M11 6L11 8.8L14.5 8.8",
  // Jenth: top bar swooping into a blade that lands on a left foot.
  J: "M4 7L11 7C15.5 7 18 9.2 19.5 12L11 19L4 19",
  // Mern: top bar, long fall to the lower left, short foot.
  M: "M10 6L19.5 6L5.5 18L15 18",
  // Osk: trapezoid outline, wider at the base.
  O: "M7.5 6L16.5 6L20 18L4 18Z",
  // Peth: U open at the top, left lip hooked inward.
  P: "M18 6L18 18L8 18C5 18 4.5 16.5 4.5 14L4.5 10C4.5 7.5 5 6 8 6L9.5 6",
  // Resh: plain "7" — top bar, diagonal leg.
  R: "M4.5 6L19.5 6L10 18",
  // Senth: long fall into a right stem, detached tick lower left.
  S: "M4.5 6L13.5 18L18 18M18 6L18 18M4.5 14L9 17.2",
  // Xesh: triangle outline.
  X: "M12 6L19 18L5 18Z",
  // Yirt: V with a notched upper-left arm.
  Y: "M4.5 6L12 18L19.5 6M4.5 6L9 6",
};

export function Aurebesh({ text = "JAMPGAME PROXY RS" }) {
  return (
    <span className="aurebesh">
      {[...text].map((ch, i) =>
        ch === " " ? (
          <span key={i} className="ab-space" />
        ) : (
          <svg
            key={i}
            viewBox="0 0 24 24"
            width="19"
            height="19"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
            focusable="false"
          >
            <path d={GLYPHS[ch]} />
          </svg>
        ),
      )}
    </span>
  );
}
