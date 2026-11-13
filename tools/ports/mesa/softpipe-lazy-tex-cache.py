# Copyright (c) 2026 tre4surehunter9
# softpipe makes a texture tile cache for every sampler slot of every
# shader stage when a context is created: 6 x 128 caches of 256 KiB, whose
# 16 tiles each are touched to mark them invalid. That's 192 MiB of
# address space and about 48 MiB of memory per OpenGL context before
# anything is drawn (glxgears and glxinfo died in 128 MiB). Here a slot's
# cache is made when a texture is first bound to it; empty slots have none.
# Run in Mesa's source directory by tools/ports/mesa.sh.
import sys

SP = "src/gallium/drivers/softpipe/"


def patch(path, old, new):
    s = open(SP + path).read()
    if new in s:
        return  # (already patched)
    assert s.count(old) == 1, (path, old[:60])
    open(SP + path, "w").write(s.replace(old, new))


# 1. No caches when the context is made.
patch("sp_context.c",
      """   /* Allocate texture caches */
   for (sh = 0; sh < ARRAY_SIZE(softpipe->tex_cache); sh++) {
      for (i = 0; i < ARRAY_SIZE(softpipe->tex_cache[0]); i++) {
         softpipe->tex_cache[sh][i] = sp_create_tex_tile_cache(&softpipe->pipe);
         if (!softpipe->tex_cache[sh][i])
            goto fail;
      }
   }
""",
      """   /* Texture caches: made when a texture is first bound to a slot
    * (PalladiumOS: softpipe-lazy-tex-cache.py). */
""")

# 2. Binding a texture makes its slot's cache.
patch("sp_state_sampler.c",
      """      pipe_sampler_view_reference(pview, views[i]);
      sp_tex_tile_cache_set_sampler_view(softpipe->tex_cache[shader][start + i],
                                         views[i]);""",
      """      pipe_sampler_view_reference(pview, views[i]);
      if (views[i] && !softpipe->tex_cache[shader][start + i])
         softpipe->tex_cache[shader][start + i] = sp_create_tex_tile_cache(pipe);
      sp_tex_tile_cache_set_sampler_view(softpipe->tex_cache[shader][start + i],
                                         views[i]);""")

# 3. Slots without a cache: nothing to set or flush.
patch("sp_tex_tile_cache.c",
      """   struct pipe_resource *texture = view ? view->texture : NULL;
   uint i;
""",
      """   struct pipe_resource *texture = view ? view->texture : NULL;
   uint i;

   if (!tc)
      return; /* (an empty slot: no cache) */
""")
patch("sp_tex_tile_cache.c",
      """sp_flush_tex_tile_cache(struct softpipe_tex_tile_cache *tc)
{
   int pos;
""",
      """sp_flush_tex_tile_cache(struct softpipe_tex_tile_cache *tc)
{
   int pos;

   if (!tc)
      return; /* (an empty slot: no cache) */
""")
print("softpipe: texture caches made on first use", file=sys.stderr)
