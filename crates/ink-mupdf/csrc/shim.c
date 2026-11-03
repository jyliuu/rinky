/* rinky <-> MuPDF shim. See shim.h. AGPL-3.0-or-later. */
#include "mupdf/fitz.h"
#include "shim.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct ink_doc {
	fz_context *ctx;
	fz_document *doc;
	int warnings;
	int print_warnings;
	char err[512];
};

static void copy_msg(char *dst, size_t len, const char *msg)
{
	if (!dst || len == 0)
		return;
	if (!msg)
		msg = "unknown MuPDF error";
	snprintf(dst, len, "%s", msg);
}

/* Error/warning callbacks: keep MuPDF quiet on stderr unless asked. */
static void on_error(void *user, const char *message)
{
	ink_doc *d = user;
	if (d && d->print_warnings)
		fprintf(stderr, "mupdf error: %s\n", message);
}

static void on_warning(void *user, const char *message)
{
	ink_doc *d = user;
	if (!d)
		return;
	d->warnings++;
	if (d->print_warnings)
		fprintf(stderr, "mupdf warning: %s\n", message);
}

/* Inside fz_catch: remember the message and mark the error as handled. */
static void caught(ink_doc *d)
{
	copy_msg(d->err, sizeof d->err, fz_caught_message(d->ctx));
	fz_report_error(d->ctx);
}

const char *ink_mupdf_version(void)
{
	return FZ_VERSION;
}

ink_doc *ink_open(const char *path, size_t store_max, char *err, size_t errlen)
{
	ink_doc *d = calloc(1, sizeof *d);
	if (!d)
	{
		copy_msg(err, errlen, "out of memory");
		return NULL;
	}
	d->print_warnings = getenv("INK_MUPDF_WARNINGS") != NULL;
	d->ctx = fz_new_context(NULL, NULL, store_max ? store_max : FZ_STORE_DEFAULT);
	if (!d->ctx)
	{
		copy_msg(err, errlen, "cannot create MuPDF context");
		free(d);
		return NULL;
	}
	fz_set_error_callback(d->ctx, on_error, d);
	fz_set_warning_callback(d->ctx, on_warning, d);
	fz_try(d->ctx)
	{
		fz_register_document_handlers(d->ctx);
		d->doc = fz_open_document(d->ctx, path);
	}
	fz_catch(d->ctx)
	{
		copy_msg(err, errlen, fz_caught_message(d->ctx));
		fz_report_error(d->ctx);
		fz_drop_context(d->ctx);
		free(d);
		return NULL;
	}
	return d;
}

void ink_close(ink_doc *d)
{
	if (!d)
		return;
	fz_drop_document(d->ctx, d->doc);
	fz_drop_context(d->ctx);
	free(d);
}

const char *ink_error(ink_doc *d)
{
	return d ? d->err : "no document";
}

int ink_warning_count(ink_doc *d)
{
	return d ? d->warnings : 0;
}

int ink_needs_password(ink_doc *d)
{
	int r = 0;
	fz_try(d->ctx)
		r = fz_needs_password(d->ctx, d->doc);
	fz_catch(d->ctx)
	{
		caught(d);
		r = -1;
	}
	return r;
}

int ink_authenticate(ink_doc *d, const char *password)
{
	int r = 0;
	fz_try(d->ctx)
		r = fz_authenticate_password(d->ctx, d->doc, password);
	fz_catch(d->ctx)
	{
		caught(d);
		r = -1;
	}
	return r;
}

int ink_page_count(ink_doc *d)
{
	int n = 0;
	fz_try(d->ctx)
		n = fz_count_pages(d->ctx, d->doc);
	fz_catch(d->ctx)
	{
		caught(d);
		n = -1;
	}
	return n;
}

int ink_metadata(ink_doc *d, const char *key, char *buf, size_t size)
{
	int r = 0;
	if (buf && size)
		buf[0] = 0;
	fz_try(d->ctx)
		r = fz_lookup_metadata(d->ctx, d->doc, key, buf, size);
	fz_catch(d->ctx)
	{
		caught(d);
		return -1;
	}
	return r < 0 ? 0 : r;
}

void ink_set_aa(ink_doc *d, int bits)
{
	fz_set_aa_level(d->ctx, bits);
}

int ink_page_bound(ink_doc *d, int page, ink_rect *out)
{
	fz_page *p = NULL;
	fz_rect r = fz_empty_rect;
	fz_var(p);
	fz_try(d->ctx)
	{
		p = fz_load_page(d->ctx, d->doc, page);
		r = fz_bound_page(d->ctx, p);
	}
	fz_always(d->ctx)
		fz_drop_page(d->ctx, p);
	fz_catch(d->ctx)
	{
		caught(d);
		return -1;
	}
	out->x0 = r.x0;
	out->y0 = r.y0;
	out->x1 = r.x1;
	out->y1 = r.y1;
	return 0;
}

ink_list *ink_list_new(ink_doc *d, int page, ink_rect *bounds)
{
	fz_display_list *list = NULL;
	fz_try(d->ctx)
	{
		list = fz_new_display_list_from_page_number(d->ctx, d->doc, page);
		if (bounds)
		{
			fz_rect r = fz_bound_display_list(d->ctx, list);
			bounds->x0 = r.x0;
			bounds->y0 = r.y0;
			bounds->x1 = r.x1;
			bounds->y1 = r.y1;
		}
	}
	fz_catch(d->ctx)
	{
		caught(d);
		return NULL;
	}
	return list;
}

void ink_list_drop(ink_doc *d, ink_list *list)
{
	if (d && list)
		fz_drop_display_list(d->ctx, list);
}

int ink_render(ink_doc *d, ink_list *list, const float m[6], int x, int y, int w, int h, unsigned char *dst)
{
	fz_pixmap *pix = NULL;
	fz_device *dev = NULL;
	fz_var(pix);
	fz_var(dev);
	if (w <= 0 || h <= 0 || !dst)
		return 0;
	fz_try(d->ctx)
	{
		fz_irect bbox = fz_make_irect(x, y, x + w, y + h);
		fz_matrix ctm = fz_make_matrix(m[0], m[1], m[2], m[3], m[4], m[5]);
		pix = fz_new_pixmap_with_bbox_and_data(d->ctx, fz_device_gray(d->ctx), bbox, NULL, 0, dst);
		fz_clear_pixmap_with_value(d->ctx, pix, 255);
		dev = fz_new_draw_device(d->ctx, fz_identity, pix);
		fz_run_display_list(d->ctx, list, dev, ctm, fz_rect_from_irect(bbox), NULL);
		fz_close_device(d->ctx, dev);
	}
	fz_always(d->ctx)
	{
		fz_drop_device(d->ctx, dev);
		fz_drop_pixmap(d->ctx, pix);
	}
	fz_catch(d->ctx)
	{
		caught(d);
		return -1;
	}
	return 0;
}

typedef struct {
	ink_char *v;
	int n, cap;
	int line, block;
	int oom;
} char_vec;

static void push_char(char_vec *cv, const fz_stext_char *ch)
{
	fz_rect r;
	if (cv->oom)
		return;
	if (cv->n == cv->cap)
	{
		int cap = cv->cap ? cv->cap * 2 : 1024;
		ink_char *v = realloc(cv->v, (size_t)cap * sizeof *v);
		if (!v)
		{
			cv->oom = 1;
			return;
		}
		cv->v = v;
		cv->cap = cap;
	}
	r = fz_rect_from_quad(ch->quad);
	cv->v[cv->n].c = ch->c;
	cv->v[cv->n].x0 = r.x0;
	cv->v[cv->n].y0 = r.y0;
	cv->v[cv->n].x1 = r.x1;
	cv->v[cv->n].y1 = r.y1;
	cv->v[cv->n].line = cv->line;
	cv->v[cv->n].block = cv->block;
	cv->n++;
}

static void collect_blocks(char_vec *cv, fz_stext_block *block)
{
	for (; block; block = block->next)
	{
		if (block->type == FZ_STEXT_BLOCK_TEXT)
		{
			fz_stext_line *line;
			for (line = block->u.t.first_line; line; line = line->next)
			{
				fz_stext_char *ch;
				for (ch = line->first_char; ch; ch = ch->next)
					push_char(cv, ch);
				cv->line++;
			}
			cv->block++;
		}
		else if (block->type == FZ_STEXT_BLOCK_STRUCT && block->u.s.down)
			collect_blocks(cv, block->u.s.down->first_block);
	}
}

int ink_text(ink_doc *d, ink_list *list, ink_char **out, int *n)
{
	fz_stext_page *text = NULL;
	char_vec cv = { 0 };
	fz_var(text);
	fz_var(cv);
	*out = NULL;
	*n = 0;
	fz_try(d->ctx)
	{
		fz_stext_options opts = { 0 };
		opts.flags = FZ_STEXT_MEDIABOX_CLIP;
		text = fz_new_stext_page_from_display_list(d->ctx, list, &opts);
		collect_blocks(&cv, text->first_block);
	}
	fz_always(d->ctx)
		fz_drop_stext_page(d->ctx, text);
	fz_catch(d->ctx)
	{
		caught(d);
		free(cv.v);
		return -1;
	}
	if (cv.oom)
	{
		free(cv.v);
		copy_msg(d->err, sizeof d->err, "out of memory collecting text");
		return -1;
	}
	*out = cv.v;
	*n = cv.n;
	return 0;
}

void ink_free(void *p)
{
	free(p);
}

static char *dup_str(const char *s)
{
	size_t len;
	char *r;
	if (!s)
		return NULL;
	len = strlen(s);
	r = malloc(len + 1);
	if (r)
		memcpy(r, s, len + 1);
	return r;
}

typedef struct {
	ink_outline_item *v;
	int n, cap;
	int oom;
} outline_vec;

static void flatten_outline(ink_doc *d, outline_vec *ov, fz_outline *node, int level)
{
	for (; node; node = node->next)
	{
		ink_outline_item *it;
		if (ov->oom)
			return;
		if (ov->n == ov->cap)
		{
			int cap = ov->cap ? ov->cap * 2 : 64;
			ink_outline_item *v = realloc(ov->v, (size_t)cap * sizeof *v);
			if (!v)
			{
				ov->oom = 1;
				return;
			}
			ov->v = v;
			ov->cap = cap;
		}
		it = &ov->v[ov->n++];
		it->level = level;
		it->title = dup_str(node->title ? node->title : "");
		it->uri = dup_str(node->uri);
		it->x = node->x;
		it->y = node->y;
		it->page = -1;
		if (node->page.page >= 0)
			it->page = fz_page_number_from_location(d->ctx, d->doc, node->page);
		else if (node->uri && !fz_is_external_link(d->ctx, node->uri))
		{
			float x = 0, y = 0;
			fz_location loc = fz_resolve_link(d->ctx, d->doc, node->uri, &x, &y);
			if (loc.page >= 0)
			{
				it->page = fz_page_number_from_location(d->ctx, d->doc, loc);
				it->x = x;
				it->y = y;
			}
		}
		if (!it->title)
			ov->oom = 1;
		if (node->down)
			flatten_outline(d, ov, node->down, level + 1);
	}
}

int ink_outline(ink_doc *d, ink_outline_item **out, int *n)
{
	fz_outline *outline = NULL;
	outline_vec ov = { 0 };
	fz_var(outline);
	fz_var(ov);
	*out = NULL;
	*n = 0;
	fz_try(d->ctx)
	{
		outline = fz_load_outline(d->ctx, d->doc);
		flatten_outline(d, &ov, outline, 0);
	}
	fz_always(d->ctx)
		fz_drop_outline(d->ctx, outline);
	fz_catch(d->ctx)
	{
		caught(d);
		ink_outline_free(ov.v, ov.n);
		return -1;
	}
	if (ov.oom)
	{
		ink_outline_free(ov.v, ov.n);
		copy_msg(d->err, sizeof d->err, "out of memory reading the outline");
		return -1;
	}
	*out = ov.v;
	*n = ov.n;
	return 0;
}

void ink_outline_free(ink_outline_item *items, int n)
{
	int i;
	if (!items)
		return;
	for (i = 0; i < n; i++)
	{
		free(items[i].title);
		free(items[i].uri);
	}
	free(items);
}

int ink_links(ink_doc *d, int page, ink_link_item **out, int *n)
{
	fz_page *p = NULL;
	fz_link *links = NULL;
	ink_link_item *v = NULL;
	int count = 0, cap = 0, oom = 0;
	fz_var(p);
	fz_var(links);
	fz_var(v);
	fz_var(count);
	fz_var(cap);
	fz_var(oom);
	*out = NULL;
	*n = 0;
	fz_try(d->ctx)
	{
		fz_link *l;
		p = fz_load_page(d->ctx, d->doc, page);
		links = fz_load_links(d->ctx, p);
		for (l = links; l && !oom; l = l->next)
		{
			ink_link_item *it;
			if (!l->uri)
				continue;
			if (count == cap)
			{
				int ncap = cap ? cap * 2 : 16;
				ink_link_item *nv = realloc(v, (size_t)ncap * sizeof *nv);
				if (!nv)
				{
					oom = 1;
					break;
				}
				v = nv;
				cap = ncap;
			}
			it = &v[count++];
			it->x0 = l->rect.x0;
			it->y0 = l->rect.y0;
			it->x1 = l->rect.x1;
			it->y1 = l->rect.y1;
			it->uri = dup_str(l->uri);
			it->external = fz_is_external_link(d->ctx, l->uri);
			it->page = -1;
			it->x = it->y = 0;
			if (!it->uri)
				oom = 1;
			else if (!it->external)
			{
				float x = 0, y = 0;
				fz_location loc = fz_resolve_link(d->ctx, d->doc, l->uri, &x, &y);
				if (loc.page >= 0)
				{
					it->page = fz_page_number_from_location(d->ctx, d->doc, loc);
					it->x = x;
					it->y = y;
				}
			}
		}
	}
	fz_always(d->ctx)
	{
		fz_drop_link(d->ctx, links);
		fz_drop_page(d->ctx, p);
	}
	fz_catch(d->ctx)
	{
		caught(d);
		ink_links_free(v, count);
		return -1;
	}
	if (oom)
	{
		ink_links_free(v, count);
		copy_msg(d->err, sizeof d->err, "out of memory reading links");
		return -1;
	}
	*out = v;
	*n = count;
	return 0;
}

void ink_links_free(ink_link_item *items, int n)
{
	int i;
	if (!items)
		return;
	for (i = 0; i < n; i++)
		free(items[i].uri);
	free(items);
}
