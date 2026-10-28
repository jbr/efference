export interface GrainSettings {
  base: readonly [number, number, number];
  lightness: number;
  color: number;
  preserve: boolean;
  scale: number;
  size: { width: number; height: number; ratio: number };
}

const vertexSource = `#version 300 es
void main() {
  vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

const fragmentSource = `#version 300 es
precision highp float;
precision highp int;
uniform vec3 u_base;
uniform vec2 u_amount;
uniform vec2 u_resolution;
uniform float u_pixelRatio;
uniform float u_scale;
uniform bool u_preserve;
out vec4 outColor;

vec3 linearRgb(vec3 v) {
  return mix(v / 12.92, pow((v + 0.055) / 1.055, vec3(2.4)),
    step(vec3(0.04045), v));
}
vec3 encodedRgb(vec3 v) {
  v = clamp(v, 0.0, 1.0);
  return mix(v * 12.92, 1.055 * pow(v, vec3(1.0 / 2.4)) - 0.055,
    step(vec3(0.0031308), v));
}
vec3 toLab(vec3 rgb) {
  vec3 v = linearRgb(rgb);
  vec3 lms = pow(vec3(
    dot(v, vec3(0.4122214708, 0.5363325363, 0.0514459929)),
    dot(v, vec3(0.2119034982, 0.6806995451, 0.1073969566)),
    dot(v, vec3(0.0883024619, 0.2817188376, 0.6299787005))
  ), vec3(1.0 / 3.0));
  return vec3(
    dot(lms, vec3(0.2104542553, 0.7936177850, -0.0040720468)),
    dot(lms, vec3(1.9779984951, -2.4285922050, 0.4505937099)),
    dot(lms, vec3(0.0259040371, 0.7827717662, -0.8086757660))
  );
}
vec3 fromLab(vec3 lab) {
  vec3 lms = vec3(
    lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z,
    lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z,
    lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z
  );
  lms = lms * lms * lms;
  return vec3(
    dot(lms, vec3(4.0767416621, -3.3077115913, 0.2309699292)),
    dot(lms, vec3(-1.2684380046, 2.6097574011, -0.3413193965)),
    dot(lms, vec3(-0.0041960863, -0.7034186147, 1.7076147010))
  );
}
float randomValue(ivec2 p, uint seed) {
  uint n = (uint(p.x) + seed) * 374761393u + uint(p.y) * 668265263u;
  n = (n ^ (n >> 13)) * 1274126177u;
  return float(n ^ (n >> 16)) / 4294967295.0 * 2.0 - 1.0;
}
float noise(vec2 position, uint seed) {
  float value = 0.0;
  for (int octave = 0; octave < 3; octave++) {
    float frequency = exp2(float(octave));
    vec2 extent = u_resolution / (u_pixelRatio * u_scale);
    ivec2 period = max(ivec2(1), ivec2(round(extent / 4.0 * frequency)));
    vec2 p = position / extent * vec2(period);
    ivec2 cell = ivec2(floor(p));
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    uint octaveSeed = seed + uint(octave) * 101u;
    ivec2 next = (cell + ivec2(1)) % period;
    cell = cell % period;
    float top = mix(randomValue(cell, octaveSeed),
      randomValue(ivec2(next.x, cell.y), octaveSeed), f.x);
    float bottom = mix(randomValue(ivec2(cell.x, next.y), octaveSeed),
      randomValue(next, octaveSeed), f.x);
    value += mix(top, bottom, f.y) / frequency;
  }
  return value;
}
float centeredNoise(vec2 p, uint seed) {
  return (noise(p, seed) - noise(p, seed + 1009u)) / sqrt(2.0);
}
bool inGamut(vec3 v) {
  return all(greaterThanEqual(v, vec3(-0.0000001))) &&
    all(lessThanEqual(v, vec3(1.0000001)));
}
void main() {
  if (u_preserve && (all(equal(u_base, vec3(1.0))) ||
      all(equal(u_base, vec3(0.0))))) {
    outColor = vec4(u_base, 1.0);
    return;
  }
  vec2 p = vec2(gl_FragCoord.x, u_resolution.y - gl_FragCoord.y)
    / (u_pixelRatio * u_scale);
  vec3 center = toLab(u_base);
  vec3 delta = vec3(centeredNoise(p, 17u) * u_amount.x,
    centeredNoise(p, 53u) * u_amount.y,
    centeredNoise(p, 97u) * u_amount.y);
  float strength = 1.0;
  if (u_preserve && (!inGamut(fromLab(center + delta)) ||
      !inGamut(fromLab(center - delta)))) {
    float low = 0.0;
    float high = 1.0;
    for (int stepIndex = 0; stepIndex < 12; stepIndex++) {
      float mid = (low + high) * 0.5;
      if (inGamut(fromLab(center + delta * mid)) &&
          inGamut(fromLab(center - delta * mid))) low = mid;
      else high = mid;
    }
    strength = low;
  }
  outColor = vec4(encodedRgb(fromLab(center + delta * strength)), 1.0);
}`;

/** One context renders static surface backgrounds (docs/web-client.md).
 * Each result is copied before the next surface reuses the GPU canvas. */
export class GrainShader {
  private readonly source = document.createElement("canvas");
  private readonly gl: WebGL2RenderingContext;
  private readonly program: WebGLProgram;
  private readonly uniforms: Record<string, WebGLUniformLocation> = {};

  constructor() {
    const gl = this.source.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
    });
    if (!gl) throw new Error("WebGL 2 is unavailable; showing flat surfaces.");
    this.gl = gl;
    try {
      const compile = (type: number, source: string) => {
        const shader = gl.createShader(type);
        if (!shader) throw new Error("Could not create the grain shader.");
        gl.shaderSource(shader, source);
        gl.compileShader(shader);
        if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
          const detail = gl.getShaderInfoLog(shader);
          gl.deleteShader(shader);
          throw new Error(`Grain shader compilation failed: ${detail}`);
        }
        return shader;
      };
      const vertex = compile(gl.VERTEX_SHADER, vertexSource);
      const fragment = compile(gl.FRAGMENT_SHADER, fragmentSource);
      const program = gl.createProgram();
      if (!program) throw new Error("Could not create the grain program.");
      gl.attachShader(program, vertex);
      gl.attachShader(program, fragment);
      gl.linkProgram(program);
      gl.deleteShader(vertex);
      gl.deleteShader(fragment);
      if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
        const detail = gl.getProgramInfoLog(program);
        gl.deleteProgram(program);
        throw new Error(`Grain shader linking failed: ${detail}`);
      }
      this.program = program;
      for (const name of [
        "base",
        "amount",
        "resolution",
        "pixelRatio",
        "scale",
        "preserve",
      ]) {
        const location = gl.getUniformLocation(program, `u_${name}`);
        if (location === null)
          throw new Error(`Missing grain uniform: ${name}`);
        this.uniforms[name] = location;
      }
    } catch (error) {
      gl.getExtension("WEBGL_lose_context")?.loseContext();
      throw error;
    }
  }

  draw(target: HTMLCanvasElement, settings: GrainSettings) {
    const gl = this.gl;
    if (gl.isContextLost())
      throw new Error("Grain renderer lost its GPU context.");
    const { width, height, ratio } = settings.size;
    this.source.width = target.width = width;
    this.source.height = target.height = height;
    gl.viewport(0, 0, width, height);
    gl.useProgram(this.program);
    gl.disable(gl.DITHER);
    gl.uniform3f(
      this.uniforms.base,
      settings.base[0] / 255,
      settings.base[1] / 255,
      settings.base[2] / 255,
    );
    gl.uniform2f(this.uniforms.amount, settings.lightness, settings.color);
    gl.uniform2f(this.uniforms.resolution, width, height);
    gl.uniform1f(this.uniforms.pixelRatio, ratio);
    gl.uniform1f(this.uniforms.scale, settings.scale);
    gl.uniform1i(this.uniforms.preserve, Number(settings.preserve));
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    const context = target.getContext("2d");
    if (!context) throw new Error("Could not copy the grain background.");
    context.drawImage(this.source, 0, 0);
  }

  destroy() {
    this.gl.deleteProgram(this.program);
    this.gl.getExtension("WEBGL_lose_context")?.loseContext();
    this.source.width = this.source.height = 0;
  }
}
