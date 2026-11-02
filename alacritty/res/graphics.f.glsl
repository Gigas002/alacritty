#if defined(GLES2_RENDERER)
#define FRAG_COLOR gl_FragColor
#define TEXTURE texture2D

varying mediump vec2 texCoords;
#else
#define FRAG_COLOR FragColor
#define TEXTURE texture

out vec4 FragColor;

in vec2 texCoords;
#endif

uniform sampler2D image;

void main() {
#if defined(GLES2_RENDERER)
    mediump vec4 color = TEXTURE(image, texCoords);
#else
    vec4 color = TEXTURE(image, texCoords);
#endif

    // Images use straight alpha, while the framebuffer uses premultiplied alpha.
    FRAG_COLOR = vec4(color.rgb * color.a, color.a);
}
