/*
 * QEMU TCG plugin for the 32-byte corruption on the emulator:
 * watches guest stores to physical page offsets 0xd0..0xef.
 *   A seq vcpu pc vaddr paddr      a user-mode store of 0x5A bytes armed the page (the
 *                                  memsentinel example fills its buffers with 0x5A)
 *   W seq vcpu pc vaddr paddr size value
 *                                  the first later store of anything else into that window
 *                                  of an armed page (then the page is disarmed)
 * Device DMA (dma_memory_write) is not a CPU store and is not seen.
 * Build (macOS host, QEMU source tree Q):
 *   clang -O2 -fPIC -I$Q/include/plugins $(pkg-config --cflags glib-2.0) -bundle \
 *     -Wl,-undefined,dynamic_lookup \
 *     -o zerotrace.dylib zerotrace.c
 * Use: -plugin file=zerotrace.dylib,out=/path/trace.txt[,mode=trace|memnop|insn]
 *   mode=trace (default) logs as above; mode=memnop registers the same store callbacks
 *   with an empty body (no log); mode=insn registers an empty per-instruction callback
 *   and no memory callbacks (a slowdown without store instrumentation).
 */
#include <inttypes.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <qemu-plugin.h>

QEMU_PLUGIN_EXPORT int qemu_plugin_version = QEMU_PLUGIN_VERSION;

#define MAX_PAGES (1u << 20) /* 4 GiB of guest physical address space */
static uint8_t armed[MAX_PAGES];
static FILE *out;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static uint64_t seq;
static enum { MODE_TRACE, MODE_MEMNOP, MODE_INSN } mode = MODE_TRACE;

static uint64_t value_of(qemu_plugin_mem_value v)
{
    switch (v.type) {
    case QEMU_PLUGIN_MEM_VALUE_U8: return v.data.u8;
    case QEMU_PLUGIN_MEM_VALUE_U16: return v.data.u16;
    case QEMU_PLUGIN_MEM_VALUE_U32: return v.data.u32;
    case QEMU_PLUGIN_MEM_VALUE_U64: return v.data.u64;
    default: return v.data.u128.low;
    }
}

static int all_5a(uint64_t v, unsigned size)
{
    for (unsigned i = 0; i < size && i < 8; i++) {
        if (((v >> (8 * i)) & 0xff) != 0x5a) {
            return 0;
        }
    }
    return 1;
}

static void on_store(unsigned int vcpu, qemu_plugin_meminfo_t info,
                     uint64_t vaddr, void *udata)
{
    struct qemu_plugin_hwaddr *hw;
    uint64_t pa, page, pc = (uint64_t)(uintptr_t)udata, val;
    unsigned size, off;

    if (!qemu_plugin_mem_is_store(info)) {
        return;
    }
    hw = qemu_plugin_get_hwaddr(info, vaddr);
    if (!hw || qemu_plugin_hwaddr_is_io(hw)) {
        return;
    }
    pa = qemu_plugin_hwaddr_phys_addr(hw);
    size = 1u << qemu_plugin_mem_size_shift(info);
    off = pa & 0xfff;
    if (!(off < 0xf0 && off + size > 0xd0)) {
        return;
    }
    page = pa >> 12;
    if (page >= MAX_PAGES) {
        return;
    }
    val = value_of(qemu_plugin_mem_get_value(info));
    if (pc < 0xc0000000u && all_5a(val, size)) {
        if (!__atomic_exchange_n(&armed[page], 1, __ATOMIC_RELAXED)) {
            pthread_mutex_lock(&lock);
            fprintf(out, "A %" PRIu64 " %u %08" PRIx64 " %08" PRIx64 " %09" PRIx64 "\n",
                    ++seq, vcpu, pc, vaddr, pa);
            pthread_mutex_unlock(&lock);
        }
        return;
    }
    if (__atomic_exchange_n(&armed[page], 0, __ATOMIC_RELAXED)) {
        pthread_mutex_lock(&lock);
        fprintf(out, "W %" PRIu64 " %u %08" PRIx64 " %08" PRIx64 " %09" PRIx64 " %u %" PRIx64 "\n",
                ++seq, vcpu, pc, vaddr, pa, size, val);
        pthread_mutex_unlock(&lock);
    }
}

static void on_store_nop(unsigned int vcpu, qemu_plugin_meminfo_t info,
                         uint64_t vaddr, void *udata)
{
}

static void on_insn_nop(unsigned int vcpu, void *udata)
{
}

static void on_tb(qemu_plugin_id_t id, struct qemu_plugin_tb *tb)
{
    size_t n = qemu_plugin_tb_n_insns(tb);

    for (size_t i = 0; i < n; i++) {
        struct qemu_plugin_insn *insn = qemu_plugin_tb_get_insn(tb, i);
        if (mode == MODE_INSN) {
            qemu_plugin_register_vcpu_insn_exec_cb(insn, on_insn_nop,
                                                   QEMU_PLUGIN_CB_NO_REGS, NULL);
            continue;
        }
        qemu_plugin_register_vcpu_mem_cb(insn,
                                         mode == MODE_MEMNOP ? on_store_nop : on_store,
                                         QEMU_PLUGIN_CB_NO_REGS, QEMU_PLUGIN_MEM_W,
                                         (void *)(uintptr_t)qemu_plugin_insn_vaddr(insn));
    }
}

static void on_exit(qemu_plugin_id_t id, void *p)
{
    pthread_mutex_lock(&lock);
    fflush(out);
    pthread_mutex_unlock(&lock);
}

QEMU_PLUGIN_EXPORT int qemu_plugin_install(qemu_plugin_id_t id,
                                           const qemu_info_t *info,
                                           int argc, char **argv)
{
    const char *path = "zerotrace.txt";

    for (int i = 0; i < argc; i++) {
        if (strncmp(argv[i], "out=", 4) == 0) {
            path = argv[i] + 4;
        } else if (strcmp(argv[i], "mode=memnop") == 0) {
            mode = MODE_MEMNOP;
        } else if (strcmp(argv[i], "mode=insn") == 0) {
            mode = MODE_INSN;
        }
    }
    out = fopen(path, "w");
    if (!out) {
        return -1;
    }
    setvbuf(out, NULL, _IOFBF, 1 << 20);
    qemu_plugin_register_vcpu_tb_trans_cb(id, on_tb);
    qemu_plugin_register_atexit_cb(id, on_exit, NULL);
    return 0;
}
