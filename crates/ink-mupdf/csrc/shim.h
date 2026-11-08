/* rinky <-> MuPDF shim.
 *
 * MuPDF reports errors with setjmp/longjmp (fz_try/fz_catch). A longjmp across Rust
 * frames is undefined behaviour, so Rust never calls a fz_* function directly: every
 * function here runs MuPDF inside fz_try, catches every error, stores the message in the
 * document handle and returns -1 (or NULL). Nothing here calls back into Rust.
 *
 * One ink_doc owns one fz_context and one fz_document. A handle is used by one thread at a
 * time (MuPDF needs no locks then).
 *
 * License: AGPL-3.0-or-later (as rinky and MuPDF).
 */
#ifndef INK_MUPDF_SHIM_H
#define INK_MUPDF_SHIM_H

#include <stddef.h>

typedef struct ink_doc ink_doc;
typedef struct fz_display_list ink_list;

typedef struct { float x0, y0, x1, y1; } ink_rect;

/* One character of structured text, in page space (points, y down). */
typedef struct {
	int c;              /* Unicode code point */
	float x0, y0, x1, y1; /* bounding box of the character quad */
	int line;           /* line number on the page, 0-based, in MuPDF's reading order */
} ink_char;

/* One outline (TOC) entry, depth-first order. */
typedef struct {
	int level;          /* 0 = top level */
	int page;           /* 0-based page, -1 if unresolved */
	float x, y;         /* target point on that page */
	char *title;        /* UTF-8, never NULL */
	char *uri;          /* may be NULL */
} ink_outline_item;

/* One link on a page. */
typedef struct {
	float x0, y0, x1, y1; /* hot area in page space */
	int external;       /* 1 = URL, 0 = inside the document */
	int page;           /* internal target page (0-based), -1 if unresolved */
	float x, y;         /* internal target point */
	char *uri;          /* never NULL */
} ink_link_item;

const char *ink_mupdf_version(void);

/* Opens a document. store_max = MuPDF resource store cap in bytes (0 = MuPDF default).
 * On failure returns NULL and writes a message into err. */
ink_doc *ink_open(const char *path, size_t store_max, char *err, size_t errlen);
void ink_close(ink_doc *d);
/* Message of the last failed call on d. */
const char *ink_error(ink_doc *d);
/* Number of warnings MuPDF printed for d so far (they go to stderr only when
 * INK_MUPDF_WARNINGS is set). */
int ink_warning_count(ink_doc *d);

int ink_needs_password(ink_doc *d);
int ink_authenticate(ink_doc *d, const char *password);
int ink_page_count(ink_doc *d);
/* Writes the value of a metadata key (info:Title, info:Author, format, ...). Returns the
 * length needed including the terminator, 0 if the key is missing, -1 on error. */
int ink_metadata(ink_doc *d, const char *key, char *buf, size_t size);
/* Anti-aliasing bits 0..8 for graphics and text. */

int ink_page_bound(ink_doc *d, int page, ink_rect *out);

/* Display list of a page (everything it draws, annotations included). */
ink_list *ink_list_new(ink_doc *d, int page, ink_rect *bounds);
void ink_list_drop(ink_doc *d, ink_list *list);

/* Draws list with matrix m = {a, b, c, d, e, f} into an 8-bit gray buffer dst of w x h
 * bytes (stride w) that covers device pixels [x, x + w) x [y, y + h). The buffer is
 * cleared to white first. */
int ink_render(ink_doc *d, ink_list *list, const float m[6], int x, int y, int w, int h, unsigned char *dst);

/* Structured text of a list. *out is malloc'ed (free with ink_free); *n characters. */
int ink_text(ink_doc *d, ink_list *list, ink_char **out, int *n);
void ink_free(void *p);

int ink_outline(ink_doc *d, ink_outline_item **out, int *n);
void ink_outline_free(ink_outline_item *items, int n);

int ink_links(ink_doc *d, int page, ink_link_item **out, int *n);
void ink_links_free(ink_link_item *items, int n);

#endif
