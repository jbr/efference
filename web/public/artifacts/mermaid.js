/* global window, document, console, getComputedStyle, MutationObserver */

// A ```mermaid fence's page (docs/artifacts.md). The host writes the fence's
// source into the document as JSON, then this script, which draws it with the
// built-in Mermaid in the theme's colours, and again whenever the theme
// changes: Mermaid writes colours into the diagram rather than referring to
// the stylesheet's variables, so a diagram drawn in one theme stays in it.
(async () => {
  const root = document.documentElement;
  const target = document.getElementById("efference-mermaid");
  const source = JSON.parse(
    document.getElementById("efference-mermaid-source").textContent,
  );

  // Mermaid derives shades from the colours it is given, and parses only hex,
  // rgb and hsl. The tokens include oklch, color-mix and translucent values,
  // so each is painted over the page's colour and read back as opaque sRGB.
  const pixel = document.createElement("canvas");
  pixel.width = 1;
  pixel.height = 1;
  const paint = pixel.getContext("2d", { willReadFrequently: true });
  const opaque = (color, under) => {
    paint.clearRect(0, 0, 1, 1);
    for (const layer of [under, color]) {
      if (layer === undefined) continue;
      paint.fillStyle = "#000";
      paint.fillStyle = layer;
      paint.fillRect(0, 0, 1, 1);
    }
    const [r, g, b] = paint.getImageData(0, 0, 1, 1).data;
    return `#${[r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
  };

  const themeVariables = () => {
    const style = getComputedStyle(root);
    const token = (name) => style.getPropertyValue(name).trim();
    const page = opaque(token("--bg-page"));
    const color = (name) => opaque(token(name), page);
    const series = [1, 2, 3, 4, 5, 6].map((n) => color(`--series-${n}`));
    const variables = {
      darkMode: style.colorScheme === "dark",
      background: page,
      fontFamily: token("--font-ui"),
      fontSize: "14px",
      primaryColor: color("--bg-raised"),
      primaryTextColor: color("--fg"),
      primaryBorderColor: color("--fg-faint"),
      secondaryColor: color("--bg-inset"),
      secondaryTextColor: color("--fg"),
      secondaryBorderColor: color("--fg-faint"),
      tertiaryColor: color("--bg-inset"),
      tertiaryTextColor: color("--fg"),
      tertiaryBorderColor: color("--border-strong"),
      lineColor: color("--fg-dim"),
      textColor: color("--fg"),
      titleColor: color("--fg-bright"),
      edgeLabelBackground: page,
      clusterBkg: color("--bg-inset"),
      // Entity attribute rows, white and light grey whatever the theme says
      // unless given.
      attributeBackgroundColorOdd: color("--bg-raised"),
      attributeBackgroundColorEven: color("--bg-inset"),
      clusterBorder: color("--border-strong"),
      noteBkgColor: color("--bg-inset"),
      noteTextColor: color("--fg"),
      noteBorderColor: color("--fg-faint"),
      // Gantt bars are drawn as nodes are, with the state in the border.
      taskBkgColor: color("--bg-raised"),
      taskBorderColor: color("--fg-dim"),
      taskTextColor: color("--fg"),
      taskTextDarkColor: color("--fg"),
      taskTextLightColor: color("--fg"),
      taskTextOutsideColor: color("--fg"),
      activeTaskBkgColor: color("--bg-raised"),
      activeTaskBorderColor: color("--accent"),
      doneTaskBkgColor: color("--bg-inset"),
      doneTaskBorderColor: color("--fg-faint"),
      critBkgColor: color("--bg-raised"),
      critBorderColor: color("--warn"),
      todayLineColor: color("--accent"),
      gridColor: color("--border"),
      sectionBkgColor: color("--bg-inset"),
      sectionBkgColor2: color("--bg-inset"),
      altSectionBkgColor: page,
      // Sections, slices and branches take the data series, in order, with
      // the page's colour for text on them.
      pieOpacity: "1",
      pieStrokeColor: page,
      pieOuterStrokeColor: page,
      pieSectionTextColor: page,
      pieTitleTextColor: color("--fg-bright"),
      pieLegendTextColor: color("--fg"),
      // Mermaid fills in this object's fields only when none is given.
      xyChart: {
        backgroundColor: page,
        titleColor: color("--fg-bright"),
        xAxisTitleColor: color("--fg"),
        xAxisLabelColor: color("--fg-dim"),
        xAxisTickColor: color("--fg-faint"),
        xAxisLineColor: color("--fg-faint"),
        yAxisTitleColor: color("--fg"),
        yAxisLabelColor: color("--fg-dim"),
        yAxisTickColor: color("--fg-faint"),
        yAxisLineColor: color("--fg-faint"),
        plotColorPalette: series.join(","),
      },
    };
    series.forEach((hue, i) => {
      for (const n of [i, i + 6]) {
        variables[`cScale${n}`] = hue;
        // The rule under a mindmap or timeline node, drawn in the inverse of
        // its fill unless it is given one.
        variables[`cScaleInv${n}`] = hue;
        variables[`cScaleLabel${n}`] = page;
        variables[`pie${n + 1}`] = hue;
      }
      variables[`git${i}`] = hue;
      variables[`gitBranchLabel${i}`] = page;
    });
    return variables;
  };

  // A diagram that does not parse shows why, where the operator reads it: the
  // agent that wrote it cannot see the page.
  const refused = (error) => {
    const line = document.createElement("p");
    line.className = "efference-notice";
    line.textContent = "This Mermaid diagram did not draw:";
    const detail = document.createElement("pre");
    detail.textContent = String(error?.message ?? error);
    target.replaceChildren(line, detail);
    console.error(error);
  };

  let mermaid;
  try {
    mermaid = await window.efference.load("mermaid");
  } catch {
    return; // The runtime has already said so on the page.
  }

  let drawn = 0;
  const draw = async () => {
    const variables = themeVariables();
    // Mermaid sizes boxes by measuring their labels, so the face has to be
    // there first.
    await document.fonts?.load(`14px ${variables.fontFamily}`);
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      suppressErrorRendering: true,
      theme: "base",
      themeVariables: variables,
      // The entity-relationship markers fill their circles white whatever the
      // theme says, and a timeline strokes its connectors in the colour of the
      // labels on its sections, which here is the page's.
      themeCSS: `marker[id*="ZERO_OR"] circle { fill: ${variables.background}; }
        .lineWrapper line { stroke: ${variables.lineColor}; }`,
    });
    drawn += 1;
    try {
      const { svg, bindFunctions } = await mermaid.render(
        `efference-mermaid-${drawn}`,
        source,
      );
      // eslint-disable-next-line no-restricted-syntax -- an artifact document: an opaque origin with no credential to reach, drawing source that is the agent's already (docs/artifacts.md).
      target.innerHTML = svg;
      bindFunctions?.(target);
    } catch (error) {
      refused(error);
    }
  };

  // One drawing at a time, in order: a theme switch mid-draw draws again after.
  let drawing = draw();
  new MutationObserver(() => {
    drawing = drawing.then(draw);
  }).observe(root, { attributes: true, attributeFilter: ["data-theme"] });
})();
