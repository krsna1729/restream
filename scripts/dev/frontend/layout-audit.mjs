// Layout checks run inside a page (Playwright `page.evaluate(auditLayout,
// { phone })`): the polish regressions a screenshot shows but no unit test
// sees. Shared by the screenshot matrix (`shots.mjs`) and the e2e layout
// spec, so what a review catches by eye stays caught.
//
// - horizontalScroll: the page scrolls sideways; overflowing names the
//   elements past its right edge, innermost last.
// - clipped: text cut by its own box (not an ellipsis, not a scroller).
// - selectClipped: a select whose chosen option is wider than the room
//   left of its arrow, so the text runs under the arrow.
// - hairlineScroll: a scroller only a few pixels taller or wider than its
//   box: a stray scrollbar from a border or margin, not real content.
// - misaligned: controls on one line of one row that differ in height or
//   sit off each other's centre line.
// - smallTargets (phones): touch targets under 36 px.
// - lowContrast: text below WCAG AA against what is actually behind it
//   (4.5:1, or 3:1 from 24 px / 18.66 px bold); placeholders and disabled
//   controls are exempt, as WCAG exempts them.
//
// Must stay self-contained: Playwright serialises the function's source.
export function auditLayout({ phone }) {
  const doc = document.documentElement;
  const visible = (element) => {
    const style = getComputedStyle(element);
    if (style.display === "none" || style.visibility === "hidden") return null;
    // What the browser does not render: a closed <details> (newer Chromium
    // keeps its content laid out under content-visibility: hidden),
    // content-visibility, visibility, or opacity 0 anywhere above.
    if (element.closest("details:not([open])") && !element.closest("summary")) return null;
    if (element.checkVisibility && !element.checkVisibility({ contentVisibilityAuto: true, opacityProperty: true, visibilityProperty: true })) return null;
    const rect = element.getBoundingClientRect();
    return rect.width === 0 || rect.height === 0 ? null : { style, rect };
  };
  const name = (element) =>
    `${element.tagName.toLowerCase()} "${(element.getAttribute("aria-label") || element.textContent || "").trim().replace(/\s+/g, " ").slice(0, 32)}"`;
  const clipped = [];
  const selectClipped = [];
  const hairlineScroll = [];
  const misaligned = [];
  const smallTargets = [];
  const lowContrast = [];
  // Any CSS colour (oklch, color-mix…) to sRGB through a 1 px canvas.
  const paint = document.createElement("canvas");
  paint.width = paint.height = 1;
  const pen = paint.getContext("2d", { willReadFrequently: true });
  const rgba = (color) => {
    pen.clearRect(0, 0, 1, 1);
    pen.fillStyle = "#000";
    pen.fillStyle = color;
    pen.fillRect(0, 0, 1, 1);
    const [r, g, b, a] = pen.getImageData(0, 0, 1, 1).data;
    return [r, g, b, a / 255];
  };
  const over = (top, under) => top.slice(0, 3).map((value, index) => value * top[3] + under[index] * (1 - top[3]));
  // What is behind an element: its own and its ancestors' backgrounds,
  // composited from the page up.
  const backdrop = (element) => {
    const layers = [];
    for (let node = element; node; node = node.parentElement) {
      const layer = rgba(getComputedStyle(node).backgroundColor);
      if (layer[3] > 0) layers.push(layer);
      if (layer[3] >= 1) break;
    }
    let color = [255, 255, 255];
    for (const layer of layers.reverse()) color = over(layer, color);
    return color;
  };
  const luminance = ([r, g, b]) =>
    [r, g, b]
      .map((value) => value / 255)
      .map((value) => (value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4))
      .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  const ratio = (a, b) => {
    const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x);
    return (high + 0.05) / (low + 0.05);
  };
  const overflowing = [];
  const measure = document.createElement("canvas").getContext("2d");
  const CONTROL = "button, select, input:not([type=checkbox]):not([type=radio]):not([type=range]):not([type=hidden]), a.btn, [role=button]";

  for (const element of document.querySelectorAll("body *")) {
    const seen = visible(element);
    if (!seen) continue;
    const { style, rect } = seen;
    // What sticks out past the page's right edge, outside any scroller.
    if (rect.right > doc.clientWidth + 1 && rect.left < doc.clientWidth) {
      let scroller = element.parentElement;
      while (scroller && scroller !== document.body && !/(auto|scroll|hidden|clip)/.test(getComputedStyle(scroller).overflowX)) {
        scroller = scroller.parentElement;
      }
      if (!scroller || scroller === document.body) overflowing.push(`${name(element)} right ${Math.round(rect.right)}`);
    }
    const hasText = [...element.childNodes].some((node) => node.nodeType === 3 && node.textContent.trim().length > 0);
    if (
      hasText &&
      style.overflowX !== "visible" === false &&
      element.scrollWidth > element.clientWidth + 2 &&
      style.textOverflow !== "ellipsis" &&
      style.overflowX !== "auto" &&
      style.overflowX !== "scroll"
    ) {
      clipped.push(element.textContent.trim().slice(0, 40));
    }
    const ownText =
      hasText ||
      element.tagName === "SELECT" ||
      element.tagName === "TEXTAREA" ||
      (element.tagName === "INPUT" && !["checkbox", "radio", "range", "color", "hidden"].includes(element.type) && element.value);
    if (ownText && !element.closest(":disabled, [aria-disabled=true], .btn-disabled, [aria-hidden=true]")) {
      const background = backdrop(element);
      const ink = rgba(style.color);
      // Opacity on the element or an ancestor fades the text into its backdrop.
      let fade = 1;
      for (let node = element; node; node = node.parentElement) fade *= Number(getComputedStyle(node).opacity);
      // Nothing to read while it is hidden by opacity (a closed menu).
      if (fade < 0.05) continue;
      const text = over([...ink.slice(0, 3), ink[3] * fade], background);
      const size = parseFloat(style.fontSize);
      const large = size >= 24 || (size >= 18.66 && Number(style.fontWeight) >= 700);
      const contrast = ratio(text, background);
      if (contrast < (large ? 3 : 4.5)) {
        lowContrast.push(`${name(element)} ${contrast.toFixed(2)}:1 rgb(${text.map(Math.round)}) on rgb(${background.map(Math.round)})`);
      }
    }
    if (element.tagName === "SELECT" && !element.multiple && element.selectedOptions[0]) {
      measure.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
      const text = measure.measureText(element.selectedOptions[0].textContent.trim()).width;
      const room = element.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
      if (text > room + 1) selectClipped.push(`${name(element)} ${Math.round(text)}>${Math.round(room)}`);
    }
    for (const [axis, overflow, extra] of [
      ["y", style.overflowY, element.scrollHeight - element.clientHeight],
      ["x", style.overflowX, element.scrollWidth - element.clientWidth],
    ]) {
      if ((overflow === "auto" || overflow === "scroll") && extra > 0 && extra <= 4 && element !== doc && element !== document.body) {
        hairlineScroll.push(`${name(element)} ${axis}+${extra}`);
      }
    }
    if (
      phone &&
      element.matches("a[href], button, select, input, [role=button], [role=tab]") &&
      // WCAG 2.5.8 exempts links inline in text.
      !(element.tagName === "A" && style.display === "inline") &&
      element.type !== "checkbox" &&
      element.type !== "radio" &&
      element.type !== "range" &&
      (rect.height < 35.5 || rect.width < 35.5) &&
      // A control inside a touch-sized label is hit through it.
      !((target) => target && target.getBoundingClientRect().height >= 35.5)(element.closest("label.touch-36")) &&
      !element.closest("[data-dense-ok]")
    ) {
      smallTargets.push(`${name(element)} ${Math.round(rect.width)}x${Math.round(rect.height)}`);
    }
    // A flex row of controls: each line's controls share a height and a centre.
    if (style.display.endsWith("flex") && style.flexDirection === "row" && !element.closest("[data-align-ok]")) {
      const controls = [...element.children].filter((child) => child.matches(CONTROL) && visible(child));
      const lines = [];
      for (const control of controls) {
        const box = control.getBoundingClientRect();
        const line = lines.find((members) => {
          const first = members[0].getBoundingClientRect();
          return box.top < first.bottom && first.top < box.bottom;
        });
        if (line) line.push(control);
        else lines.push([control]);
      }
      for (const line of lines) {
        if (line.length < 2) continue;
        const boxes = line.map((control) => control.getBoundingClientRect());
        const heights = boxes.map((box) => box.height);
        const centres = boxes.map((box) => box.top + box.height / 2);
        if (Math.max(...heights) - Math.min(...heights) > 2 || Math.max(...centres) - Math.min(...centres) > 2) {
          misaligned.push(line.map((control, index) => `${name(control)} h${Math.round(heights[index])}`).join(" | "));
        }
      }
    }
  }
  return {
    horizontalScroll: doc.scrollWidth > doc.clientWidth + 1,
    overflowing: overflowing.slice(-6),
    scrollWidth: doc.scrollWidth,
    clientWidth: doc.clientWidth,
    clipped,
    selectClipped,
    hairlineScroll,
    misaligned,
    smallTargets,
    lowContrast,
    domNodes: document.getElementsByTagName("*").length,
  };
}
