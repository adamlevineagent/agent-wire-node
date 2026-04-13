import type { PyramidRenderer } from './PyramidRenderer';
import type { SurfaceNode, SurfaceEdge, NodeEncoding, OverlayState, HitTestResult, VizPrimitive, BuildVizState } from './types';
import { NodeVisualState, EdgeCategory } from './types';

// ── Node color map (RGBA float arrays for GPU upload) ──────────────

/** RGBA as floats [0-1] with base alpha. */
interface ColorF {
    r: number;
    g: number;
    b: number;
    a: number;
}

function rgba(r: number, g: number, b: number, a: number): ColorF {
    return { r: r / 255, g: g / 255, b: b / 255, a };
}

const NODE_COLORS_F: Record<string, { fill: ColorF; hover: ColorF }> = {
    [NodeVisualState.STABLE]: {
        fill: rgba(34, 211, 238, 0.4),
        hover: rgba(34, 211, 238, 0.7),
    },
    [NodeVisualState.STALE_CONFIRMED]: {
        fill: rgba(64, 208, 128, 0.9),
        hover: rgba(64, 208, 128, 1.0),
    },
    [NodeVisualState.JUST_UPDATED]: {
        fill: rgba(64, 208, 128, 0.7),
        hover: rgba(64, 208, 128, 0.9),
    },
    [NodeVisualState.NOT_STALE]: {
        fill: rgba(72, 230, 255, 0.82),
        hover: rgba(120, 240, 255, 0.98),
    },
    [NodeVisualState.BUILD_COMPLETE]: {
        fill: rgba(34, 211, 238, 0.7),
        hover: rgba(34, 211, 238, 0.85),
    },
    [NodeVisualState.BUILD_FAILED]: {
        fill: rgba(255, 100, 100, 0.7),
        hover: rgba(255, 100, 100, 0.85),
    },
    [NodeVisualState.BUILDING]: {
        fill: rgba(34, 211, 238, 0.3),
        hover: rgba(34, 211, 238, 0.5),
    },
    [NodeVisualState.CACHED]: {
        fill: rgba(34, 211, 238, 0.5),
        hover: rgba(34, 211, 238, 0.65),
    },
};

const BEDROCK_FILL_F: ColorF = rgba(120, 160, 180, 0.3);
const BEDROCK_HOVER_F: ColorF = rgba(120, 160, 180, 0.55);

// ── Edge style config (float colors) ───────────────────────────────

interface EdgeStyleF {
    color: ColorF;
    lineWidth: number;
    dashed: boolean;
}

const EDGE_STYLES_F: Record<string, EdgeStyleF> = {
    [EdgeCategory.STRUCTURAL]: {
        color: rgba(34, 211, 238, 0.15),
        lineWidth: 0.5,
        dashed: false,
    },
    [EdgeCategory.BEDROCK]: {
        color: rgba(120, 160, 180, 0.1),
        lineWidth: 0.5,
        dashed: false,
    },
    [EdgeCategory.WEB]: {
        color: rgba(168, 85, 247, 0.25),
        lineWidth: 0.5,
        dashed: true,
    },
    [EdgeCategory.EVIDENCE]: {
        color: rgba(34, 211, 238, 0.35),
        lineWidth: 1,
        dashed: false,
    },
};

// ── Bezier tessellation ────────────────────────────────────────────

const BEZIER_SEGMENTS = 8;

/** Tessellate a quadratic bezier into line segments.
 *  Returns flat array of [x0,y0, x1,y1, x2,y2, ...] */
function tessellateQuadBezier(
    x0: number, y0: number,
    cx: number, cy: number,
    x1: number, y1: number,
): Float32Array {
    const pts = new Float32Array((BEZIER_SEGMENTS + 1) * 2);
    for (let i = 0; i <= BEZIER_SEGMENTS; i++) {
        const t = i / BEZIER_SEGMENTS;
        const mt = 1 - t;
        pts[i * 2] = mt * mt * x0 + 2 * mt * t * cx + t * t * x1;
        pts[i * 2 + 1] = mt * mt * y0 + 2 * mt * t * cy + t * t * y1;
    }
    return pts;
}

// ── HSL helpers (for saturation modulation on CPU) ─────────────────

function rgbToHsl(r: number, g: number, b: number): { h: number; s: number; l: number } {
    const max = Math.max(r, g, b), min = Math.min(r, g, b);
    const l = (max + min) / 2;
    let h = 0, s = 0;
    if (max !== min) {
        const d = max - min;
        s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
        switch (max) {
            case r: h = ((g - b) / d + (g < b ? 6 : 0)) / 6; break;
            case g: h = ((b - r) / d + 2) / 6; break;
            case b: h = ((r - g) / d + 4) / 6; break;
        }
    }
    return { h, s, l };
}

function hslToRgb(h: number, s: number, l: number): { r: number; g: number; b: number } {
    if (s === 0) return { r: l, g: l, b: l };
    const hue2rgb = (p: number, q: number, t: number): number => {
        if (t < 0) t += 1;
        if (t > 1) t -= 1;
        if (t < 1 / 6) return p + (q - p) * 6 * t;
        if (t < 1 / 2) return q;
        if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
        return p;
    };
    const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
    const p = 2 * l - q;
    return {
        r: hue2rgb(p, q, h + 1 / 3),
        g: hue2rgb(p, q, h),
        b: hue2rgb(p, q, h - 1 / 3),
    };
}

/** Modulate saturation of an rgba float color. factor 0=grayscale, 1=original. */
function modulateSaturation(c: ColorF, factor: number): ColorF {
    const hsl = rgbToHsl(c.r, c.g, c.b);
    hsl.s *= Math.max(0, Math.min(1, factor));
    const rgb = hslToRgb(hsl.h, hsl.s, hsl.l);
    return { r: rgb.r, g: rgb.g, b: rgb.b, a: c.a };
}

// ── GLSL shader sources ────────────────────────────────────────────

const NODE_VERT_SRC = `#version 300 es
precision highp float;

// Per-vertex quad (unit square centered at origin: -1..1)
in vec2 a_position;

// Per-instance attributes
in vec2 a_offset;       // node center in CSS pixels
in float a_radius;      // radius in CSS pixels
in vec4 a_color;        // fill rgba [0-1]
in vec3 a_encoding;     // x=brightness, y=saturation (unused here, applied on CPU), z=borderThickness
in float a_glowStrength; // 0 = no glow, >0 = glow radius multiplier

uniform vec2 u_resolution; // viewport in CSS pixels
uniform float u_pulsePhase;

out vec2 v_localPos;    // [-1,1] quad-space position
out vec4 v_color;
out float v_borderThickness;
out float v_glowStrength;
out float v_radiusPx;

void main() {
    v_localPos = a_position;
    v_color = a_color;
    v_borderThickness = a_encoding.z;
    v_glowStrength = a_glowStrength;
    v_radiusPx = a_radius;

    // Expand quad slightly beyond node radius for glow falloff
    float expand = a_glowStrength > 0.0 ? 1.5 : 1.15;
    vec2 worldPos = a_offset + a_position * a_radius * expand;

    // Map from CSS-pixel coords to clip space [-1,1]
    vec2 ndc = (worldPos / u_resolution) * 2.0 - 1.0;
    ndc.y = -ndc.y; // flip Y (CSS Y-down to GL Y-up)
    gl_Position = vec4(ndc, 0.0, 1.0);
}
`;

const NODE_FRAG_SRC = `#version 300 es
precision highp float;

in vec2 v_localPos;
in vec4 v_color;
in float v_borderThickness;
in float v_glowStrength;
in float v_radiusPx;

out vec4 fragColor;

void main() {
    float dist = length(v_localPos);

    // Outside the expanded quad (including glow margin)
    float expand = v_glowStrength > 0.0 ? 1.5 : 1.15;
    if (dist > expand) discard;

    // SDF circle at radius 1.0 in local space
    // Smooth edge with antialiasing
    float edgeWidth = 1.5 / max(v_radiusPx, 1.0); // ~1.5px AA band in local space
    float circleAlpha = 1.0 - smoothstep(1.0 - edgeWidth, 1.0 + edgeWidth, dist);

    vec4 col = v_color;

    // Border ring (encoding axis 3)
    if (v_borderThickness > 0.0) {
        float borderWidthLocal = v_borderThickness * 3.0 / max(v_radiusPx, 1.0);
        float borderInner = 1.0 - borderWidthLocal;
        float borderAlpha = smoothstep(borderInner - edgeWidth, borderInner, dist)
                          * (1.0 - smoothstep(1.0, 1.0 + edgeWidth, dist));
        vec4 borderColor = vec4(34.0/255.0, 211.0/255.0, 238.0/255.0, 0.6);
        col = mix(col, borderColor, borderAlpha * 0.8);
    }

    // Glow: soft falloff outside the circle edge
    if (v_glowStrength > 0.0 && dist > 1.0) {
        float glowDist = (dist - 1.0) / (expand - 1.0);
        float glowAlpha = (1.0 - glowDist * glowDist) * v_glowStrength * 0.4;
        fragColor = vec4(col.rgb, glowAlpha);
        return;
    }

    fragColor = vec4(col.rgb, col.a * circleAlpha);
}
`;

const EDGE_VERT_SRC = `#version 300 es
precision highp float;

in vec2 a_position;
in vec4 a_color;

uniform vec2 u_resolution;

out vec4 v_color;

void main() {
    v_color = a_color;
    vec2 ndc = (a_position / u_resolution) * 2.0 - 1.0;
    ndc.y = -ndc.y;
    gl_Position = vec4(ndc, 0.0, 1.0);
}
`;

const EDGE_FRAG_SRC = `#version 300 es
precision highp float;

in vec4 v_color;
out vec4 fragColor;

void main() {
    fragColor = v_color;
}
`;

// Fullscreen quad for bloom composite pass
const BLOOM_VERT_SRC = `#version 300 es
precision highp float;

layout(location = 0) in vec2 a_position;
out vec2 v_uv;

void main() {
    v_uv = a_position * 0.5 + 0.5;
    gl_Position = vec4(a_position, 0.0, 1.0);
}
`;

const BLOOM_BLUR_FRAG_SRC = `#version 300 es
precision highp float;

uniform sampler2D u_texture;
uniform vec2 u_direction; // (1/w, 0) or (0, 1/h)
in vec2 v_uv;
out vec4 fragColor;

void main() {
    // 5-tap gaussian blur
    float weights[5] = float[](0.227027, 0.1945946, 0.1216216, 0.054054, 0.016216);
    vec4 result = texture(u_texture, v_uv) * weights[0];
    for (int i = 1; i < 5; i++) {
        vec2 offset = u_direction * float(i);
        result += texture(u_texture, v_uv + offset) * weights[i];
        result += texture(u_texture, v_uv - offset) * weights[i];
    }
    fragColor = result;
}
`;

const BLOOM_COMPOSITE_FRAG_SRC = `#version 300 es
precision highp float;

uniform sampler2D u_scene;
uniform sampler2D u_bloom;
in vec2 v_uv;
out vec4 fragColor;

void main() {
    vec4 scene = texture(u_scene, v_uv);
    vec4 bloom = texture(u_bloom, v_uv);
    // Additive blend, clamped
    fragColor = vec4(min(scene.rgb + bloom.rgb * 0.6, vec3(1.0)), scene.a);
}
`;

// ── Shader compilation helpers ─────────────────────────────────────

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
    const shader = gl.createShader(type);
    if (!shader) throw new Error('GpuRenderer: failed to create shader');
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        const info = gl.getShaderInfoLog(shader) ?? 'unknown';
        gl.deleteShader(shader);
        throw new Error(`GpuRenderer: shader compile error: ${info}`);
    }
    return shader;
}

function linkProgram(
    gl: WebGL2RenderingContext,
    vertSrc: string,
    fragSrc: string,
): WebGLProgram {
    const vert = compileShader(gl, gl.VERTEX_SHADER, vertSrc);
    const frag = compileShader(gl, gl.FRAGMENT_SHADER, fragSrc);
    const prog = gl.createProgram();
    if (!prog) throw new Error('GpuRenderer: failed to create program');
    gl.attachShader(prog, vert);
    gl.attachShader(prog, frag);
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
        const info = gl.getProgramInfoLog(prog) ?? 'unknown';
        gl.deleteProgram(prog);
        throw new Error(`GpuRenderer: program link error: ${info}`);
    }
    // Shaders can be detached after linking
    gl.detachShader(prog, vert);
    gl.detachShader(prog, frag);
    gl.deleteShader(vert);
    gl.deleteShader(frag);
    return prog;
}

// ── Unit quad geometry ─────────────────────────────────────────────

/** Two triangles forming a [-1,1] quad. 6 vertices, 2 floats each. */
const QUAD_VERTS = new Float32Array([
    -1, -1,  1, -1,  1, 1,
    -1, -1,  1,  1, -1, 1,
]);

/** Fullscreen quad for post-processing: [-1,1] clip space. */
const FULLSCREEN_QUAD = new Float32Array([
    -1, -1,  1, -1,  1, 1,
    -1, -1,  1,  1, -1, 1,
]);

// ── Instance data layout ───────────────────────────────────────────
// Per-instance floats for node rendering:
// [offsetX, offsetY, radius, colorR, colorG, colorB, colorA, encBrightness, encSaturation, encBorderThick, glowStrength]
const NODE_INSTANCE_FLOATS = 11;

// Per-vertex floats for edge rendering:
// [x, y, colorR, colorG, colorB, colorA]
const EDGE_VERTEX_FLOATS = 6;

// ── GpuRenderer ────────────────────────────────────────────────────

/**
 * Rich rendering tier: WebGL2-based renderer with instanced drawing
 * for nodes, batched line geometry for edges, and optional bloom pass.
 *
 * Falls back to throwing a descriptive error if WebGL2 is unavailable,
 * so PyramidSurface can catch it and fall back to CanvasRenderer.
 */
export class GpuRenderer implements PyramidRenderer {
    private canvas: HTMLCanvasElement | null = null;
    private gl: WebGL2RenderingContext | null = null;
    private container: HTMLElement | null = null;
    private width = 0;
    private height = 0;
    private dpr = 1;
    private encodings = new Map<string, NodeEncoding>();
    private activeVizPrimitive: VizPrimitive | null = null;
    private buildVizState: BuildVizState | null = null;
    private linkIntensities = new Map<string, number>();

    // Shader programs
    private nodeProgram: WebGLProgram | null = null;
    private edgeProgram: WebGLProgram | null = null;
    private bloomBlurProgram: WebGLProgram | null = null;
    private bloomCompositeProgram: WebGLProgram | null = null;

    // VAOs
    private nodeVAO: WebGLVertexArrayObject | null = null;
    private edgeVAO: WebGLVertexArrayObject | null = null;
    private bloomVAO: WebGLVertexArrayObject | null = null;

    // Buffers
    private quadVBO: WebGLBuffer | null = null;
    private nodeInstanceVBO: WebGLBuffer | null = null;
    private edgeVBO: WebGLBuffer | null = null;
    private fullscreenQuadVBO: WebGLBuffer | null = null;

    // Bloom framebuffers (ping-pong)
    private sceneFBO: WebGLFramebuffer | null = null;
    private sceneTexture: WebGLTexture | null = null;
    private bloomFBOs: [WebGLFramebuffer | null, WebGLFramebuffer | null] = [null, null];
    private bloomTextures: [WebGLTexture | null, WebGLTexture | null] = [null, null];
    private bloomEnabled = true;

    // Uniform locations (cached)
    private nodeUniforms: { resolution: WebGLUniformLocation | null; pulsePhase: WebGLUniformLocation | null } = {
        resolution: null, pulsePhase: null,
    };
    private edgeUniforms: { resolution: WebGLUniformLocation | null } = { resolution: null };
    private bloomBlurUniforms: { texture: WebGLUniformLocation | null; direction: WebGLUniformLocation | null } = {
        texture: null, direction: null,
    };
    private bloomCompositeUniforms: { scene: WebGLUniformLocation | null; bloom: WebGLUniformLocation | null } = {
        scene: null, bloom: null,
    };

    // Animation state
    private pulsePhase = 0;
    private lastPulseTime = 0;

    // Reusable typed arrays for instance data
    private nodeInstanceData: Float32Array = new Float32Array(0);
    private edgeVertexData: Float32Array = new Float32Array(0);
    private lastNodeInstanceCount = 0;
    private lastEdgeVertexCount = 0;

    // ── Lifecycle ───────────────────────────────────────────────────

    attach(container: HTMLElement): void {
        this.container = container;

        const canvas = document.createElement('canvas');
        canvas.style.display = 'block';
        container.appendChild(canvas);
        this.canvas = canvas;

        const gl = canvas.getContext('webgl2', {
            alpha: true,
            premultipliedAlpha: false,
            antialias: true,
            powerPreference: 'high-performance',
        });
        if (!gl) {
            // Clean up canvas before throwing
            container.removeChild(canvas);
            this.canvas = null;
            throw new Error(
                'GpuRenderer: WebGL2 is not available in this browser. ' +
                'The renderer requires WebGL2 for instanced drawing and VAOs. ' +
                'Falling back to CanvasRenderer.',
            );
        }
        this.gl = gl;
        this.dpr = window.devicePixelRatio || 1;

        // Enable blending for transparent nodes/edges
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

        // Compile all shader programs
        this.initShaders(gl);

        // Set up geometry buffers and VAOs
        this.initBuffers(gl);

        // Initial size
        const rect = container.getBoundingClientRect();
        this.applySize(rect.width, rect.height);
    }

    destroy(): void {
        const gl = this.gl;
        if (gl) {
            // Delete programs
            if (this.nodeProgram) gl.deleteProgram(this.nodeProgram);
            if (this.edgeProgram) gl.deleteProgram(this.edgeProgram);
            if (this.bloomBlurProgram) gl.deleteProgram(this.bloomBlurProgram);
            if (this.bloomCompositeProgram) gl.deleteProgram(this.bloomCompositeProgram);

            // Delete VAOs
            if (this.nodeVAO) gl.deleteVertexArray(this.nodeVAO);
            if (this.edgeVAO) gl.deleteVertexArray(this.edgeVAO);
            if (this.bloomVAO) gl.deleteVertexArray(this.bloomVAO);

            // Delete buffers
            if (this.quadVBO) gl.deleteBuffer(this.quadVBO);
            if (this.nodeInstanceVBO) gl.deleteBuffer(this.nodeInstanceVBO);
            if (this.edgeVBO) gl.deleteBuffer(this.edgeVBO);
            if (this.fullscreenQuadVBO) gl.deleteBuffer(this.fullscreenQuadVBO);

            // Delete bloom resources
            this.destroyBloomResources(gl);
        }

        if (this.canvas && this.container) {
            this.container.removeChild(this.canvas);
        }
        this.canvas = null;
        this.gl = null;
        this.container = null;
        this.nodeProgram = null;
        this.edgeProgram = null;
        this.bloomBlurProgram = null;
        this.bloomCompositeProgram = null;
        this.nodeVAO = null;
        this.edgeVAO = null;
        this.bloomVAO = null;
        this.quadVBO = null;
        this.nodeInstanceVBO = null;
        this.edgeVBO = null;
        this.fullscreenQuadVBO = null;
        this.encodings.clear();
    }

    resize(width: number, height: number): void {
        this.applySize(width, height);
    }

    private applySize(width: number, height: number): void {
        if (!this.canvas || !this.gl) return;
        this.width = width;
        this.height = height;
        this.dpr = window.devicePixelRatio || 1;

        const pw = Math.round(width * this.dpr);
        const ph = Math.round(height * this.dpr);

        this.canvas.width = pw;
        this.canvas.height = ph;
        this.canvas.style.width = `${width}px`;
        this.canvas.style.height = `${height}px`;

        this.gl.viewport(0, 0, pw, ph);

        // Recreate bloom FBOs at new size
        this.initBloomFBOs(this.gl, pw, ph);
    }

    // ── Encoding ────────────────────────────────────────────────────

    setNodeEncoding(nodeId: string, encoding: NodeEncoding): void {
        this.encodings.set(nodeId, encoding);
    }

    setNodeEncodings(encodings: Map<string, NodeEncoding>): void {
        this.encodings = new Map(encodings);
    }

    setActiveVizPrimitive(primitive: VizPrimitive | null): void {
        this.activeVizPrimitive = primitive;
    }

    setBuildVizState(state: BuildVizState): void {
        this.buildVizState = state;
    }

    setLinkIntensities(intensities: Map<string, number>): void {
        this.linkIntensities = new Map(intensities);
    }

    // ── Hit testing ─────────────────────────────────────────────────

    hitTest(x: number, y: number, nodes: SurfaceNode[]): HitTestResult | null {
        if (!this.container || nodes.length === 0) return null;

        const rect = this.container.getBoundingClientRect();
        const mx = x - rect.left;
        const my = y - rect.top;

        // Reverse depth order: apex (highest depth) gets priority
        const sorted = [...nodes].sort((a, b) => b.depth - a.depth);

        for (const node of sorted) {
            const dx = mx - node.x;
            const dy = my - node.y;
            const hitRadius = node.radius + 4;
            if (dx * dx + dy * dy <= hitRadius * hitRadius) {
                return { nodeId: node.id, node };
            }
        }

        return null;
    }

    // ── Dimensions ──────────────────────────────────────────────────

    getDimensions(): { width: number; height: number } {
        return { width: this.width, height: this.height };
    }

    // ── Shader initialization ───────────────────────────────────────

    private initShaders(gl: WebGL2RenderingContext): void {
        this.nodeProgram = linkProgram(gl, NODE_VERT_SRC, NODE_FRAG_SRC);
        this.edgeProgram = linkProgram(gl, EDGE_VERT_SRC, EDGE_FRAG_SRC);

        // Cache uniform locations
        this.nodeUniforms.resolution = gl.getUniformLocation(this.nodeProgram, 'u_resolution');
        this.nodeUniforms.pulsePhase = gl.getUniformLocation(this.nodeProgram, 'u_pulsePhase');
        this.edgeUniforms.resolution = gl.getUniformLocation(this.edgeProgram, 'u_resolution');

        // Bloom shaders
        try {
            this.bloomBlurProgram = linkProgram(gl, BLOOM_VERT_SRC, BLOOM_BLUR_FRAG_SRC);
            this.bloomCompositeProgram = linkProgram(gl, BLOOM_VERT_SRC, BLOOM_COMPOSITE_FRAG_SRC);
            this.bloomBlurUniforms.texture = gl.getUniformLocation(this.bloomBlurProgram, 'u_texture');
            this.bloomBlurUniforms.direction = gl.getUniformLocation(this.bloomBlurProgram, 'u_direction');
            this.bloomCompositeUniforms.scene = gl.getUniformLocation(this.bloomCompositeProgram, 'u_scene');
            this.bloomCompositeUniforms.bloom = gl.getUniformLocation(this.bloomCompositeProgram, 'u_bloom');
        } catch {
            // Bloom is non-critical; degrade gracefully
            this.bloomEnabled = false;
            this.bloomBlurProgram = null;
            this.bloomCompositeProgram = null;
        }
    }

    // ── Buffer initialization ───────────────────────────────────────

    private initBuffers(gl: WebGL2RenderingContext): void {
        // ── Node VAO: instanced quad ────────────────────────────────
        this.nodeVAO = gl.createVertexArray();
        gl.bindVertexArray(this.nodeVAO);

        // Quad geometry (shared across all instances)
        this.quadVBO = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.quadVBO);
        gl.bufferData(gl.ARRAY_BUFFER, QUAD_VERTS, gl.STATIC_DRAW);

        const nodeAPosition = gl.getAttribLocation(this.nodeProgram!, 'a_position');
        gl.enableVertexAttribArray(nodeAPosition);
        gl.vertexAttribPointer(nodeAPosition, 2, gl.FLOAT, false, 0, 0);

        // Instance buffer (dynamic, uploaded each frame)
        this.nodeInstanceVBO = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.nodeInstanceVBO);
        // Allocate with reasonable initial capacity
        gl.bufferData(gl.ARRAY_BUFFER, NODE_INSTANCE_FLOATS * 4 * 256, gl.DYNAMIC_DRAW);

        const stride = NODE_INSTANCE_FLOATS * 4; // bytes per instance
        const setupInstanceAttrib = (name: string, size: number, offset: number): void => {
            const loc = gl.getAttribLocation(this.nodeProgram!, name);
            if (loc < 0) return; // attribute optimized out
            gl.enableVertexAttribArray(loc);
            gl.vertexAttribPointer(loc, size, gl.FLOAT, false, stride, offset * 4);
            gl.vertexAttribDivisor(loc, 1); // per-instance
        };

        // Layout: [offsetX(0), offsetY(1), radius(2), colorR(3), colorG(4), colorB(5), colorA(6),
        //          encBrightness(7), encSaturation(8), encBorderThick(9), glowStrength(10)]
        setupInstanceAttrib('a_offset', 2, 0);
        setupInstanceAttrib('a_radius', 1, 2);
        setupInstanceAttrib('a_color', 4, 3);
        setupInstanceAttrib('a_encoding', 3, 7);
        setupInstanceAttrib('a_glowStrength', 1, 10);

        gl.bindVertexArray(null);

        // ── Edge VAO: per-vertex position + color ───────────────────
        this.edgeVAO = gl.createVertexArray();
        gl.bindVertexArray(this.edgeVAO);

        this.edgeVBO = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.edgeVBO);
        gl.bufferData(gl.ARRAY_BUFFER, EDGE_VERTEX_FLOATS * 4 * 1024, gl.DYNAMIC_DRAW);

        const edgeStride = EDGE_VERTEX_FLOATS * 4;
        const edgeAPosition = gl.getAttribLocation(this.edgeProgram!, 'a_position');
        gl.enableVertexAttribArray(edgeAPosition);
        gl.vertexAttribPointer(edgeAPosition, 2, gl.FLOAT, false, edgeStride, 0);

        const edgeAColor = gl.getAttribLocation(this.edgeProgram!, 'a_color');
        gl.enableVertexAttribArray(edgeAColor);
        gl.vertexAttribPointer(edgeAColor, 4, gl.FLOAT, false, edgeStride, 2 * 4);

        gl.bindVertexArray(null);

        // ── Bloom fullscreen quad VAO ───────────────────────────────
        this.bloomVAO = gl.createVertexArray();
        gl.bindVertexArray(this.bloomVAO);

        this.fullscreenQuadVBO = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.fullscreenQuadVBO);
        gl.bufferData(gl.ARRAY_BUFFER, FULLSCREEN_QUAD, gl.STATIC_DRAW);

        // Both bloom programs use a_position at location 0
        gl.enableVertexAttribArray(0);
        gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

        gl.bindVertexArray(null);
    }

    // ── Bloom framebuffer management ────────────────────────────────

    private initBloomFBOs(gl: WebGL2RenderingContext, pw: number, ph: number): void {
        if (!this.bloomEnabled) return;

        this.destroyBloomResources(gl);

        // Scene FBO (full resolution, captures the main render for compositing)
        const sceneResult = this.createFBOWithTexture(gl, pw, ph);
        this.sceneFBO = sceneResult.fbo;
        this.sceneTexture = sceneResult.texture;

        // Bloom ping-pong FBOs at half resolution for performance
        const bw = Math.max(1, Math.floor(pw / 2));
        const bh = Math.max(1, Math.floor(ph / 2));
        const bloom0 = this.createFBOWithTexture(gl, bw, bh);
        const bloom1 = this.createFBOWithTexture(gl, bw, bh);
        this.bloomFBOs = [bloom0.fbo, bloom1.fbo];
        this.bloomTextures = [bloom0.texture, bloom1.texture];
    }

    private createFBOWithTexture(
        gl: WebGL2RenderingContext,
        w: number,
        h: number,
    ): { fbo: WebGLFramebuffer; texture: WebGLTexture } {
        const texture = gl.createTexture()!;
        gl.bindTexture(gl.TEXTURE_2D, texture);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

        const fbo = gl.createFramebuffer()!;
        gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);

        return { fbo, texture };
    }

    private destroyBloomResources(gl: WebGL2RenderingContext): void {
        if (this.sceneFBO) gl.deleteFramebuffer(this.sceneFBO);
        if (this.sceneTexture) gl.deleteTexture(this.sceneTexture);
        for (const fbo of this.bloomFBOs) { if (fbo) gl.deleteFramebuffer(fbo); }
        for (const tex of this.bloomTextures) { if (tex) gl.deleteTexture(tex); }
        this.sceneFBO = null;
        this.sceneTexture = null;
        this.bloomFBOs = [null, null];
        this.bloomTextures = [null, null];
    }

    // ── Main render ─────────────────────────────────────────────────

    render(
        nodes: SurfaceNode[],
        edges: SurfaceEdge[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        const gl = this.gl;
        if (!gl) return;

        // Update pulse phase for building nodes
        const now = performance.now();
        if (this.lastPulseTime > 0) {
            const dt = now - this.lastPulseTime;
            this.pulsePhase = (this.pulsePhase + dt * 0.003) % (Math.PI * 2);
        }
        this.lastPulseTime = now;

        const hasGlowNodes = this.bloomEnabled && nodes.some((n) =>
            n.state === NodeVisualState.STALE_CONFIRMED ||
            n.state === NodeVisualState.JUST_UPDATED ||
            n.state === NodeVisualState.NOT_STALE,
        );

        if (hasGlowNodes && this.sceneFBO) {
            // Render to scene FBO, then bloom, then composite to screen
            gl.bindFramebuffer(gl.FRAMEBUFFER, this.sceneFBO);
            gl.viewport(0, 0, this.canvas!.width, this.canvas!.height);
            this.renderScene(gl, nodes, edges, overlays, hoveredNodeId);
            gl.bindFramebuffer(gl.FRAMEBUFFER, null);

            this.renderBloom(gl, nodes, overlays, hoveredNodeId);
            this.compositeBloom(gl);
        } else {
            // No glow nodes — render directly to screen (skip bloom overhead)
            gl.bindFramebuffer(gl.FRAMEBUFFER, null);
            gl.viewport(0, 0, this.canvas!.width, this.canvas!.height);
            this.renderScene(gl, nodes, edges, overlays, hoveredNodeId);
        }
    }

    private renderScene(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
        edges: SurfaceEdge[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);

        if (nodes.length === 0) return;

        // Draw edges first (behind nodes)
        this.drawEdges(gl, edges, overlays);

        // Draw nodes (sorted by depth ascending so apex renders on top via painter's)
        this.drawNodes(gl, nodes, overlays, hoveredNodeId);

        // Draw viz primitive overlays (build-time visuals)
        this.drawVizOverlay(gl, nodes);
    }

    // ── Edge rendering ──────────────────────────────────────────────

    private drawEdges(
        gl: WebGL2RenderingContext,
        edges: SurfaceEdge[],
        overlays: OverlayState,
    ): void {
        if (edges.length === 0) return;

        // Filter edges by overlay state and build vertex data
        const filteredEdges: SurfaceEdge[] = [];
        for (const edge of edges) {
            if (edge.category === EdgeCategory.BEDROCK && !overlays.provenance) continue;
            if (edge.category === EdgeCategory.WEB && !overlays.web) continue;
            if (edge.category === EdgeCategory.EVIDENCE && !overlays.structure) continue;
            filteredEdges.push(edge);
        }

        if (filteredEdges.length === 0) return;

        // Each edge tessellated into BEZIER_SEGMENTS line segments = BEZIER_SEGMENTS * 2 vertices
        // (using GL_LINES, each segment is 2 vertices)
        const maxVertices = filteredEdges.length * BEZIER_SEGMENTS * 2;
        const neededFloats = maxVertices * EDGE_VERTEX_FLOATS;

        // Grow buffer if needed
        if (neededFloats > this.edgeVertexData.length) {
            this.edgeVertexData = new Float32Array(neededFloats);
        }

        let vertexCount = 0;
        const data = this.edgeVertexData;

        for (const edge of filteredEdges) {
            const style = EDGE_STYLES_F[edge.category] ?? EDGE_STYLES_F[EdgeCategory.STRUCTURAL];
            let c = style.color;

            // Link intensity modulation
            const intensityKey = `${edge.fromId}\u2192${edge.toId}`;
            const intensity = this.linkIntensities.get(intensityKey);
            if (intensity !== undefined && overlays.weightIntensity) {
                c = { ...c, a: 0.1 + intensity * 0.5 };
            }

            const pts = tessellateQuadBezier(
                edge.fromX, edge.fromY,
                edge.controlX, edge.controlY,
                edge.toX, edge.toY,
            );

            // For dashed edges, only draw even-numbered segments
            for (let i = 0; i < BEZIER_SEGMENTS; i++) {
                if (style.dashed && i % 2 === 1) continue;

                const baseIdx = vertexCount * EDGE_VERTEX_FLOATS;
                // Start vertex
                data[baseIdx + 0] = pts[i * 2];
                data[baseIdx + 1] = pts[i * 2 + 1];
                data[baseIdx + 2] = c.r;
                data[baseIdx + 3] = c.g;
                data[baseIdx + 4] = c.b;
                data[baseIdx + 5] = c.a;
                // End vertex
                data[baseIdx + 6] = pts[(i + 1) * 2];
                data[baseIdx + 7] = pts[(i + 1) * 2 + 1];
                data[baseIdx + 8] = c.r;
                data[baseIdx + 9] = c.g;
                data[baseIdx + 10] = c.b;
                data[baseIdx + 11] = c.a;
                vertexCount += 2;
            }
        }

        if (vertexCount === 0) return;

        gl.useProgram(this.edgeProgram);
        gl.uniform2f(this.edgeUniforms.resolution, this.width, this.height);

        gl.bindVertexArray(this.edgeVAO);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.edgeVBO);

        // Upload only the portion of the buffer we filled
        if (vertexCount > this.lastEdgeVertexCount) {
            // Buffer needs to grow — reallocate
            gl.bufferData(gl.ARRAY_BUFFER, data.subarray(0, vertexCount * EDGE_VERTEX_FLOATS), gl.DYNAMIC_DRAW);
        } else {
            gl.bufferSubData(gl.ARRAY_BUFFER, 0, data.subarray(0, vertexCount * EDGE_VERTEX_FLOATS));
        }
        this.lastEdgeVertexCount = vertexCount;

        gl.lineWidth(1.0); // WebGL2 only guarantees 1.0, but that's fine for our use
        gl.drawArrays(gl.LINES, 0, vertexCount);

        gl.bindVertexArray(null);
    }

    // ── Node rendering ──────────────────────────────────────────────

    private drawNodes(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        // Sort by depth ascending so apex renders on top
        const sorted = [...nodes].sort((a, b) => a.depth - b.depth);

        const instanceCount = sorted.length;
        const neededFloats = instanceCount * NODE_INSTANCE_FLOATS;

        if (neededFloats > this.nodeInstanceData.length) {
            this.nodeInstanceData = new Float32Array(neededFloats);
        }

        const data = this.nodeInstanceData;

        for (let i = 0; i < instanceCount; i++) {
            const node = sorted[i];
            const isHovered = node.id === hoveredNodeId;
            const isBedrock = node.depth === -1;
            const effectiveState = overlays.staleness ? node.state : NodeVisualState.STABLE;

            // Determine draw radius
            let drawRadius: number;
            if (isBedrock) {
                drawRadius = isHovered ? node.radius * 1.6 : node.radius;
            } else {
                drawRadius = isHovered ? node.radius * 1.4 : node.radius;
            }

            // Determine base color
            let color: ColorF;
            if (isBedrock) {
                color = isHovered ? { ...BEDROCK_HOVER_F } : { ...BEDROCK_FILL_F };
            } else {
                const colorPair = NODE_COLORS_F[effectiveState] ?? NODE_COLORS_F[NodeVisualState.STABLE];
                color = isHovered ? { ...colorPair.hover } : { ...colorPair.fill };
            }

            // Three-axis encoding modulation
            const encoding = overlays.weightIntensity ? this.encodings.get(node.id) : undefined;
            let encBrightness = 1;
            let encSaturation = 1;
            let encBorderThick = 0;

            if (encoding && !isBedrock) {
                // Axis 1 — Brightness modulates alpha
                color.a *= (0.3 + encoding.brightness * 0.7);
                encBrightness = encoding.brightness;

                // Axis 2 — Saturation modulates color vividness
                const satFactor = 0.2 + encoding.saturation * 0.8;
                color = modulateSaturation(color, satFactor);
                encSaturation = encoding.saturation;

                // Axis 3 — Border thickness
                encBorderThick = encoding.borderThickness;
            }

            // Building state: pulse effect
            if (effectiveState === NodeVisualState.BUILDING && !isBedrock) {
                const pulse = 0.5 + 0.5 * Math.sin(this.pulsePhase);
                color.a = Math.min(1, (0.3 + pulse * 0.4));
            }

            // Glow strength for bloom pass
            let glowStrength = 0;
            if (!isBedrock) {
                if (effectiveState === NodeVisualState.STALE_CONFIRMED ||
                    effectiveState === NodeVisualState.JUST_UPDATED) {
                    glowStrength = isHovered ? 1.0 : 0.75;
                } else if (effectiveState === NodeVisualState.NOT_STALE) {
                    glowStrength = isHovered ? 0.85 : 0.6;
                }
            }

            // Pack into instance data
            const base = i * NODE_INSTANCE_FLOATS;
            data[base + 0] = node.x;
            data[base + 1] = node.y;
            data[base + 2] = drawRadius;
            data[base + 3] = color.r;
            data[base + 4] = color.g;
            data[base + 5] = color.b;
            data[base + 6] = color.a;
            data[base + 7] = encBrightness;
            data[base + 8] = encSaturation;
            data[base + 9] = encBorderThick;
            data[base + 10] = glowStrength;
        }

        gl.useProgram(this.nodeProgram);
        gl.uniform2f(this.nodeUniforms.resolution, this.width, this.height);
        gl.uniform1f(this.nodeUniforms.pulsePhase, this.pulsePhase);

        gl.bindVertexArray(this.nodeVAO);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.nodeInstanceVBO);

        // Upload instance data
        if (instanceCount > this.lastNodeInstanceCount) {
            gl.bufferData(gl.ARRAY_BUFFER, data.subarray(0, neededFloats), gl.DYNAMIC_DRAW);
        } else {
            gl.bufferSubData(gl.ARRAY_BUFFER, 0, data.subarray(0, neededFloats));
        }
        this.lastNodeInstanceCount = instanceCount;

        // Draw all nodes in one instanced call: 6 vertices per quad, instanceCount instances
        gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, instanceCount);

        gl.bindVertexArray(null);
    }

    // ── Viz primitive overlay ───────────────────────────────────────

    private drawVizOverlay(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
    ): void {
        if (!this.activeVizPrimitive || !this.buildVizState) return;

        if (this.activeVizPrimitive === 'node_fill' || this.activeVizPrimitive === 'progress_only') {
            return;
        }

        if (this.activeVizPrimitive === 'edge_draw') {
            this.drawEdgeDrawOverlay(gl, nodes);
        } else if (this.activeVizPrimitive === 'verdict_mark') {
            this.drawVerdictMarkOverlay(gl, nodes);
        } else if (this.activeVizPrimitive === 'cluster_form') {
            this.drawClusterFormOverlay(gl, nodes);
        }
    }

    /** Draw animated new edges as cyan lines via the edge shader. */
    private drawEdgeDrawOverlay(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
    ): void {
        const newEdges = this.buildVizState?.newEdges;
        if (!newEdges || newEdges.length === 0) return;

        const fadeAlpha = 0.4 + 0.6 * (0.5 + 0.5 * Math.sin(this.pulsePhase));
        const cyan: ColorF = { r: 0, g: 1, b: 1, a: 0.7 * fadeAlpha };

        // Build line segment data for new edges (straight lines, no bezier)
        const maxVerts = newEdges.length * 2;
        const overlayData = new Float32Array(maxVerts * EDGE_VERTEX_FLOATS);
        let vertCount = 0;

        for (const edge of newEdges) {
            const source = nodes.find((n) => n.id === edge.sourceId);
            const target = nodes.find((n) => n.id === edge.targetId);
            if (!source || !target) continue;

            const base = vertCount * EDGE_VERTEX_FLOATS;
            overlayData[base + 0] = source.x;
            overlayData[base + 1] = source.y;
            overlayData[base + 2] = cyan.r;
            overlayData[base + 3] = cyan.g;
            overlayData[base + 4] = cyan.b;
            overlayData[base + 5] = cyan.a;
            overlayData[base + 6] = target.x;
            overlayData[base + 7] = target.y;
            overlayData[base + 8] = cyan.r;
            overlayData[base + 9] = cyan.g;
            overlayData[base + 10] = cyan.b;
            overlayData[base + 11] = cyan.a;
            vertCount += 2;
        }

        if (vertCount === 0) return;

        gl.useProgram(this.edgeProgram);
        gl.uniform2f(this.edgeUniforms.resolution, this.width, this.height);

        gl.bindVertexArray(this.edgeVAO);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.edgeVBO);
        gl.bufferData(gl.ARRAY_BUFFER, overlayData.subarray(0, vertCount * EDGE_VERTEX_FLOATS), gl.DYNAMIC_DRAW);
        // Reset edge vertex tracking so the next frame re-uploads the standard edge data
        this.lastEdgeVertexCount = 0;

        gl.lineWidth(1.0);
        gl.drawArrays(gl.LINES, 0, vertCount);
        gl.bindVertexArray(null);
    }

    /** Draw verdict rings as additional circle instances via the node shader. */
    private drawVerdictMarkOverlay(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
    ): void {
        const verdicts = this.buildVizState?.verdictsByNode;
        if (!verdicts || verdicts.size === 0) return;

        // Build ring instances — one per verdict node
        const ringData = new Float32Array(verdicts.size * NODE_INSTANCE_FLOATS);
        let ringCount = 0;

        for (const [nodeId, verdict] of verdicts) {
            const node = nodes.find((n) => n.id === nodeId);
            if (!node) continue;

            let color: ColorF;
            if (verdict === 'KEEP') {
                color = { r: 64 / 255, g: 208 / 255, b: 128 / 255, a: 0.8 };
            } else if (verdict === 'DISCONNECT') {
                color = { r: 1, g: 165 / 255, b: 0, a: 0.8 };
            } else {
                // MISSING: yellow pulsing
                const pulseAlpha = 0.4 + 0.4 * Math.sin(this.pulsePhase);
                color = { r: 1, g: 220 / 255, b: 50 / 255, a: pulseAlpha };
            }

            const ringRadius = node.radius + 4;
            const base = ringCount * NODE_INSTANCE_FLOATS;
            ringData[base + 0] = node.x;
            ringData[base + 1] = node.y;
            ringData[base + 2] = ringRadius;
            ringData[base + 3] = color.r;
            ringData[base + 4] = color.g;
            ringData[base + 5] = color.b;
            // Use very low fill alpha — the ring effect comes from the border
            ringData[base + 6] = 0.0;
            ringData[base + 7] = 1;
            ringData[base + 8] = 1;
            // borderThickness drives the ring in the fragment shader
            ringData[base + 9] = 0.7;
            ringData[base + 10] = 0;
            ringCount++;
        }

        if (ringCount === 0) return;

        gl.useProgram(this.nodeProgram);
        gl.uniform2f(this.nodeUniforms.resolution, this.width, this.height);
        gl.uniform1f(this.nodeUniforms.pulsePhase, this.pulsePhase);

        gl.bindVertexArray(this.nodeVAO);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.nodeInstanceVBO);
        gl.bufferData(gl.ARRAY_BUFFER, ringData.subarray(0, ringCount * NODE_INSTANCE_FLOATS), gl.DYNAMIC_DRAW);
        // Reset capacity tracking so drawNodes re-uploads properly next frame
        this.lastNodeInstanceCount = 0;

        gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, ringCount);
        gl.bindVertexArray(null);
    }

    /** Draw tinted quads behind cluster members via the edge shader (filled rectangles). */
    private drawClusterFormOverlay(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
    ): void {
        const clusters = this.buildVizState?.clusterMembers;
        if (!clusters || clusters.size === 0) return;

        const clusterKeys = Array.from(clusters.keys());
        const hueStep = 360 / Math.max(clusterKeys.length, 1);

        // Build filled quad geometry as two triangles per cluster, using the edge shader
        // (position + color per vertex). Each cluster = 6 vertices (2 triangles).
        const maxVerts = clusterKeys.length * 6;
        const quadData = new Float32Array(maxVerts * EDGE_VERTEX_FLOATS);
        let vertCount = 0;

        for (let ci = 0; ci < clusterKeys.length; ci++) {
            const memberIds = clusters.get(clusterKeys[ci]);
            if (!memberIds || memberIds.length === 0) continue;

            const memberNodes: SurfaceNode[] = [];
            for (const mid of memberIds) {
                const n = nodes.find((nd) => nd.id === mid);
                if (n) memberNodes.push(n);
            }
            if (memberNodes.length === 0) continue;

            // Compute bounding box
            let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
            for (const n of memberNodes) {
                minX = Math.min(minX, n.x - n.radius);
                minY = Math.min(minY, n.y - n.radius);
                maxX = Math.max(maxX, n.x + n.radius);
                maxY = Math.max(maxY, n.y + n.radius);
            }
            const pad = 8;
            minX -= pad; minY -= pad; maxX += pad; maxY += pad;

            // HSL to RGB for cluster hue
            const hue = (ci * hueStep) / 360;
            const rgb = hslToRgb(hue, 0.6, 0.5);
            const c: ColorF = { r: rgb.r, g: rgb.g, b: rgb.b, a: 0.06 };

            // Two triangles: (minX,minY), (maxX,minY), (maxX,maxY) and (minX,minY), (maxX,maxY), (minX,maxY)
            const writeVert = (x: number, y: number): void => {
                const base = vertCount * EDGE_VERTEX_FLOATS;
                quadData[base + 0] = x;
                quadData[base + 1] = y;
                quadData[base + 2] = c.r;
                quadData[base + 3] = c.g;
                quadData[base + 4] = c.b;
                quadData[base + 5] = c.a;
                vertCount++;
            };

            writeVert(minX, minY);
            writeVert(maxX, minY);
            writeVert(maxX, maxY);
            writeVert(minX, minY);
            writeVert(maxX, maxY);
            writeVert(minX, maxY);
        }

        if (vertCount === 0) return;

        gl.useProgram(this.edgeProgram);
        gl.uniform2f(this.edgeUniforms.resolution, this.width, this.height);

        gl.bindVertexArray(this.edgeVAO);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.edgeVBO);
        gl.bufferData(gl.ARRAY_BUFFER, quadData.subarray(0, vertCount * EDGE_VERTEX_FLOATS), gl.DYNAMIC_DRAW);
        // Reset edge vertex tracking
        this.lastEdgeVertexCount = 0;

        gl.drawArrays(gl.TRIANGLES, 0, vertCount);
        gl.bindVertexArray(null);
    }

    // ── Bloom pipeline ──────────────────────────────────────────────

    private renderBloom(
        gl: WebGL2RenderingContext,
        nodes: SurfaceNode[],
        overlays: OverlayState,
        hoveredNodeId: string | null,
    ): void {
        if (!this.bloomEnabled || !this.bloomFBOs[0] || !this.bloomTextures[0]) return;

        const bw = Math.max(1, Math.floor(this.canvas!.width / 2));
        const bh = Math.max(1, Math.floor(this.canvas!.height / 2));

        // Step 1: Render glow-only nodes to bloom FBO[0] at half resolution
        gl.bindFramebuffer(gl.FRAMEBUFFER, this.bloomFBOs[0]);
        gl.viewport(0, 0, bw, bh);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);

        // Only render nodes that have glow
        const glowNodes = nodes.filter((n) =>
            n.state === NodeVisualState.STALE_CONFIRMED ||
            n.state === NodeVisualState.JUST_UPDATED ||
            n.state === NodeVisualState.NOT_STALE,
        );

        if (glowNodes.length > 0) {
            // Resolution uniform stays at full CSS size — node positions are in
            // full CSS-pixel space; the half-res viewport just reduces raster fidelity.
            gl.useProgram(this.nodeProgram);
            gl.uniform2f(this.nodeUniforms.resolution, this.width, this.height);
            gl.uniform1f(this.nodeUniforms.pulsePhase, this.pulsePhase);

            // Build instance data for glow nodes only (brighter for bloom extraction)
            const glowInstanceData = new Float32Array(glowNodes.length * NODE_INSTANCE_FLOATS);
            for (let i = 0; i < glowNodes.length; i++) {
                const node = glowNodes[i];
                const isHovered = node.id === hoveredNodeId;
                const effectiveState = overlays.staleness ? node.state : NodeVisualState.STABLE;
                const colorPair = NODE_COLORS_F[effectiveState] ?? NODE_COLORS_F[NodeVisualState.STABLE];
                const color = isHovered ? { ...colorPair.hover } : { ...colorPair.fill };
                // Boost brightness for bloom extraction
                color.a = Math.min(1, color.a * 1.5);

                const drawRadius = (isHovered ? node.radius * 1.4 : node.radius) * 1.3; // slightly larger for bloom spread

                const base = i * NODE_INSTANCE_FLOATS;
                glowInstanceData[base + 0] = node.x;
                glowInstanceData[base + 1] = node.y;
                glowInstanceData[base + 2] = drawRadius;
                glowInstanceData[base + 3] = color.r;
                glowInstanceData[base + 4] = color.g;
                glowInstanceData[base + 5] = color.b;
                glowInstanceData[base + 6] = color.a;
                glowInstanceData[base + 7] = 1;
                glowInstanceData[base + 8] = 1;
                glowInstanceData[base + 9] = 0;
                glowInstanceData[base + 10] = 1.0;
            }

            gl.bindVertexArray(this.nodeVAO);
            gl.bindBuffer(gl.ARRAY_BUFFER, this.nodeInstanceVBO);
            gl.bufferData(gl.ARRAY_BUFFER, glowInstanceData, gl.DYNAMIC_DRAW);
            gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, glowNodes.length);
            gl.bindVertexArray(null);

            // Reset tracked capacity so drawNodes() uses bufferData (not
            // bufferSubData) on the next frame — the bloom upload above may
            // have shrunk the VBO below the full-scene size.
            this.lastNodeInstanceCount = 0;
        }

        // Step 2: Two-pass separable Gaussian blur (ping-pong between bloom FBOs)
        gl.useProgram(this.bloomBlurProgram);
        gl.bindVertexArray(this.bloomVAO);

        // Horizontal blur: read from bloomTextures[0], write to bloomFBOs[1]
        gl.bindFramebuffer(gl.FRAMEBUFFER, this.bloomFBOs[1]);
        gl.viewport(0, 0, bw, bh);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this.bloomTextures[0]);
        gl.uniform1i(this.bloomBlurUniforms.texture, 0);
        gl.uniform2f(this.bloomBlurUniforms.direction, 1.0 / bw, 0);
        gl.drawArrays(gl.TRIANGLES, 0, 6);

        // Vertical blur: read from bloomTextures[1], write to bloomFBOs[0]
        gl.bindFramebuffer(gl.FRAMEBUFFER, this.bloomFBOs[0]);
        gl.viewport(0, 0, bw, bh);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);
        gl.bindTexture(gl.TEXTURE_2D, this.bloomTextures[1]);
        gl.uniform2f(this.bloomBlurUniforms.direction, 0, 1.0 / bh);
        gl.drawArrays(gl.TRIANGLES, 0, 6);

        gl.bindVertexArray(null);
    }

    private compositeBloom(gl: WebGL2RenderingContext): void {
        if (!this.bloomEnabled || !this.bloomCompositeProgram || !this.sceneTexture || !this.bloomTextures[0]) return;

        // Composite to screen: scene + bloom
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);
        gl.viewport(0, 0, this.canvas!.width, this.canvas!.height);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);

        gl.useProgram(this.bloomCompositeProgram);
        gl.bindVertexArray(this.bloomVAO);

        // Scene texture on unit 0
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this.sceneTexture);
        gl.uniform1i(this.bloomCompositeUniforms.scene, 0);

        // Bloom texture on unit 1
        gl.activeTexture(gl.TEXTURE1);
        gl.bindTexture(gl.TEXTURE_2D, this.bloomTextures[0]);
        gl.uniform1i(this.bloomCompositeUniforms.bloom, 1);

        gl.drawArrays(gl.TRIANGLES, 0, 6);

        gl.bindVertexArray(null);
        gl.activeTexture(gl.TEXTURE0); // Reset active texture unit
    }
}
