/*
 * NEEDLE File Lock Enforcement Library (LD_PRELOAD)
 *
 * This shared library intercepts file write syscalls (open, openat) and enforces
 * NEEDLE file locks at the libc level. It provides hard enforcement for agents
 * that don't support native hooks (OpenCode, Aider).
 *
 * Lock Structure (mirrors src/lock/checkout.sh):
 *   /dev/shm/needle/{bead-id}-{path-uuid}
 *   Where path-uuid is the first 8 characters of MD5 hash of the absolute file path
 *
 * Usage:
 *   gcc -shared -fPIC -o libcheckout.so libcheckout.c -ldl
 *   LD_PRELOAD=./libcheckout.so <command>
 *
 * Environment Variables:
 *   NEEDLE_LOCK_DIR       - Override lock directory (default: /dev/shm/needle)
 *   NEEDLE_PRELOAD_DEBUG  - Set to "1" to enable debug logging
 *   NEEDLE_BEAD_ID        - Current bead ID (locks held by this bead are allowed)
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

/* Configuration */
#define LOCK_DIR_DEFAULT "/dev/shm/needle"
#define PATH_UUID_LEN 8
#define DEBUG_ENV "NEEDLE_PRELOAD_DEBUG"
#define BEAD_ID_ENV "NEEDLE_BEAD_ID"
#define LOCK_DIR_ENV "NEEDLE_LOCK_DIR"

/* Real function pointers */
static int (*real_open)(const char *, int, ...) = NULL;
static int (*real_openat)(int, const char *, int, ...) = NULL;

/* State */
static int initialized = 0;
static int debug_enabled = 0;
static char current_bead_id[64] = {0};
static char lock_dir[PATH_MAX] = LOCK_DIR_DEFAULT;

/* Debug logging */
#define DEBUG_LOG(fmt, ...) do { \
    if (debug_enabled) { \
        fprintf(stderr, "[libcheckout] " fmt "\n", ##__VA_ARGS__); \
    } \
} while(0)

/* Simple MD5 implementation for path UUID computation */
/* Using a minimal implementation to avoid external dependencies */

/* F, G, H and I are basic MD5 functions */
#define F(x, y, z) (((x) & (y)) | ((~x) & (z)))
#define G(x, y, z) (((x) & (z)) | ((y) & (~z)))
#define H(x, y, z) ((x) ^ (y) ^ (z))
#define I(x, y, z) ((y) ^ ((x) | (~z)))

/* ROTATE_LEFT rotates x left n bits */
#define ROTATE_LEFT(x, n) (((x) << (n)) | ((x) >> (32-(n))))

/* FF, GG, HH, and II transformations */
#define FF(a, b, c, d, x, s, ac) { \
    (a) += F ((b), (c), (d)) + (x) + (unsigned int)(ac); \
    (a) = ROTATE_LEFT ((a), (s)); \
    (a) += (b); \
}
#define GG(a, b, c, d, x, s, ac) { \
    (a) += G ((b), (c), (d)) + (x) + (unsigned int)(ac); \
    (a) = ROTATE_LEFT ((a), (s)); \
    (a) += (b); \
}
#define HH(a, b, c, d, x, s, ac) { \
    (a) += H ((b), (c), (d)) + (x) + (unsigned int)(ac); \
    (a) = ROTATE_LEFT ((a), (s)); \
    (a) += (b); \
}
#define II(a, b, c, d, x, s, ac) { \
    (a) += I ((b), (c), (d)) + (x) + (unsigned int)(ac); \
    (a) = ROTATE_LEFT ((a), (s)); \
    (a) += (b); \
}

static const unsigned char PADDING[64] = {
    0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
};

/* Constants for MD5Transform routine */
static const unsigned int S11 = 7, S12 = 12, S13 = 17, S14 = 22;
static const unsigned int S21 = 5, S22 = 9,  S23 = 14, S24 = 20;
static const unsigned int S31 = 4, S32 = 11, S33 = 16, S34 = 23;
static const unsigned int S41 = 6, S42 = 10, S43 = 15, S44 = 21;

/* MD5 context structure */
typedef struct {
    unsigned int state[4];    /* state (ABCD) */
    unsigned int count[2];    /* number of bits, modulo 2^64 (lsb first) */
    unsigned char buffer[64]; /* input buffer */
} MD5_CTX;

static void MD5Transform(unsigned int state[4], const unsigned char block[64]);
static void Encode(unsigned char *output, unsigned int *input, unsigned int len);
static void Decode(unsigned int *output, const unsigned char *input, unsigned int len);

/* MD5 initialization */
static void MD5Init(MD5_CTX *context) {
    context->count[0] = context->count[1] = 0;
    context->state[0] = 0x67452301;
    context->state[1] = 0xefcdab89;
    context->state[2] = 0x98badcfe;
    context->state[3] = 0x10325476;
}

/* MD5 block update operation */
static void MD5Update(MD5_CTX *context, const unsigned char *input, unsigned int inputLen) {
    unsigned int i, index, partLen;

    index = (unsigned int)((context->count[0] >> 3) & 0x3F);

    if ((context->count[0] += ((unsigned int)inputLen << 3)) < ((unsigned int)inputLen << 3))
        context->count[1]++;
    context->count[1] += ((unsigned int)inputLen >> 29);

    partLen = 64 - index;

    if (inputLen >= partLen) {
        memcpy(&context->buffer[index], input, partLen);
        MD5Transform(context->state, context->buffer);

        for (i = partLen; i + 63 < inputLen; i += 64)
            MD5Transform(context->state, &input[i]);

        index = 0;
    } else {
        i = 0;
    }

    memcpy(&context->buffer[index], &input[i], inputLen - i);
}

/* MD5 finalization */
static void MD5Final(unsigned char digest[16], MD5_CTX *context) {
    unsigned char bits[8];
    unsigned int index, padLen;

    Encode(bits, context->count, 8);

    index = (unsigned int)((context->count[0] >> 3) & 0x3f);
    padLen = (index < 56) ? (56 - index) : (120 - index);
    MD5Update(context, PADDING, padLen);

    MD5Update(context, bits, 8);
    Encode(digest, context->state, 16);

    memset(context, 0, sizeof(*context));
}

/* MD5 basic transformation */
static void MD5Transform(unsigned int state[4], const unsigned char block[64]) {
    unsigned int a = state[0], b = state[1], c = state[2], d = state[3], x[16];

    Decode(x, block, 64);

    /* Round 1 */
    FF(a, b, c, d, x[ 0], S11, 0xd76aa478);
    FF(d, a, b, c, x[ 1], S12, 0xe8c7b756);
    FF(c, d, a, b, x[ 2], S13, 0x242070db);
    FF(b, c, d, a, x[ 3], S14, 0xc1bdceee);
    FF(a, b, c, d, x[ 4], S11, 0xf57c0faf);
    FF(d, a, b, c, x[ 5], S12, 0x4787c62a);
    FF(c, d, a, b, x[ 6], S13, 0xa8304613);
    FF(b, c, d, a, x[ 7], S14, 0xfd469501);
    FF(a, b, c, d, x[ 8], S11, 0x698098d8);
    FF(d, a, b, c, x[ 9], S12, 0x8b44f7af);
    FF(c, d, a, b, x[10], S13, 0xffff5bb1);
    FF(b, c, d, a, x[11], S14, 0x895cd7be);
    FF(a, b, c, d, x[12], S11, 0x6b901122);
    FF(d, a, b, c, x[13], S12, 0xfd987193);
    FF(c, d, a, b, x[14], S13, 0xa679438e);
    FF(b, c, d, a, x[15], S14, 0x49b40821);

    /* Round 2 */
    GG(a, b, c, d, x[ 1], S21, 0xf61e2562);
    GG(d, a, b, c, x[ 6], S22, 0xc040b340);
    GG(c, d, a, b, x[11], S23, 0x265e5a51);
    GG(b, c, d, a, x[ 0], S24, 0xe9b6c7aa);
    GG(a, b, c, d, x[ 5], S21, 0xd62f105d);
    GG(d, a, b, c, x[10], S22, 0x02441453);
    GG(c, d, a, b, x[15], S23, 0xd8a1e681);
    GG(b, c, d, a, x[ 4], S24, 0xe7d3fbc8);
    GG(a, b, c, d, x[ 9], S21, 0x21e1cde6);
    GG(d, a, b, c, x[14], S22, 0xc33707d6);
    GG(c, d, a, b, x[ 3], S23, 0xf4d50d87);
    GG(b, c, d, a, x[ 8], S24, 0x455a14ed);
    GG(a, b, c, d, x[13], S21, 0xa9e3e905);
    GG(d, a, b, c, x[ 2], S22, 0xfcefa3f8);
    GG(c, d, a, b, x[ 7], S23, 0x676f02d9);
    GG(b, c, d, a, x[12], S24, 0x8d2a4c8a);

    /* Round 3 */
    HH(a, b, c, d, x[ 5], S31, 0xfffa3942);
    HH(d, a, b, c, x[ 8], S32, 0x8771f681);
    HH(c, d, a, b, x[11], S33, 0x6d9d6122);
    HH(b, c, d, a, x[14], S34, 0xfde5380c);
    HH(a, b, c, d, x[ 1], S31, 0xa4beea44);
    HH(d, a, b, c, x[ 4], S32, 0x4bdecfa9);
    HH(c, d, a, b, x[ 7], S33, 0xf6bb4b60);
    HH(b, c, d, a, x[10], S34, 0xbebfbc70);
    HH(a, b, c, d, x[13], S31, 0x289b7ec6);
    HH(d, a, b, c, x[ 0], S32, 0xeaa127fa);
    HH(c, d, a, b, x[ 3], S33, 0xd4ef3085);
    HH(b, c, d, a, x[ 6], S34, 0x04881d05);
    HH(a, b, c, d, x[ 9], S31, 0xd9d4d039);
    HH(d, a, b, c, x[12], S32, 0xe6db99e5);
    HH(c, d, a, b, x[15], S33, 0x1fa27cf8);
    HH(b, c, d, a, x[ 2], S34, 0xc4ac5665);

    /* Round 4 */
    II(a, b, c, d, x[ 0], S41, 0xf4292244);
    II(d, a, b, c, x[ 7], S42, 0x432aff97);
    II(c, d, a, b, x[14], S43, 0xab9423a7);
    II(b, c, d, a, x[ 5], S44, 0xfc93a039);
    II(a, b, c, d, x[12], S41, 0x655b59c3);
    II(d, a, b, c, x[ 3], S42, 0x8f0ccc92);
    II(c, d, a, b, x[10], S43, 0xffeff47d);
    II(b, c, d, a, x[ 1], S44, 0x85845dd1);
    II(a, b, c, d, x[ 8], S41, 0x6fa87e4f);
    II(d, a, b, c, x[15], S42, 0xfe2ce6e0);
    II(c, d, a, b, x[ 6], S43, 0xa3014314);
    II(b, c, d, a, x[13], S44, 0x4e0811a1);
    II(a, b, c, d, x[ 4], S41, 0xf7537e82);
    II(d, a, b, c, x[11], S42, 0xbd3af235);
    II(c, d, a, b, x[ 2], S43, 0x2ad7d2bb);
    II(b, c, d, a, x[ 9], S44, 0xeb86d391);

    state[0] += a;
    state[1] += b;
    state[2] += c;
    state[3] += d;

    memset(x, 0, sizeof(x));
}

static void Encode(unsigned char *output, unsigned int *input, unsigned int len) {
    unsigned int i, j;
    for (i = 0, j = 0; j < len; i++, j += 4) {
        output[j] = (unsigned char)(input[i] & 0xff);
        output[j+1] = (unsigned char)((input[i] >> 8) & 0xff);
        output[j+2] = (unsigned char)((input[i] >> 16) & 0xff);
        output[j+3] = (unsigned char)((input[i] >> 24) & 0xff);
    }
}

static void Decode(unsigned int *output, const unsigned char *input, unsigned int len) {
    unsigned int i, j;
    for (i = 0, j = 0; j < len; i++, j += 4)
        output[i] = ((unsigned int)input[j]) | (((unsigned int)input[j+1]) << 8) |
                    (((unsigned int)input[j+2]) << 16) | (((unsigned int)input[j+3]) << 24);
}

/* End of MD5 implementation */

/*
 * Compute the path UUID (first 8 chars of MD5 hash)
 * This mirrors the bash implementation in checkout.sh
 */
static void compute_path_uuid(const char *path, char *uuid_out) {
    MD5_CTX ctx;
    unsigned char digest[16];
    char abs_path[PATH_MAX];

    /* Resolve to absolute path if needed */
    if (path[0] != '/') {
        if (!realpath(path, abs_path)) {
            /* If realpath fails, use the path as-is */
            strncpy(abs_path, path, PATH_MAX - 1);
            abs_path[PATH_MAX - 1] = '\0';
        }
    } else {
        strncpy(abs_path, path, PATH_MAX - 1);
        abs_path[PATH_MAX - 1] = '\0';
    }

    /* Compute MD5 hash */
    MD5Init(&ctx);
    MD5Update(&ctx, (unsigned char *)abs_path, strlen(abs_path));
    MD5Final(digest, &ctx);

    /* Convert first 4 bytes to hex (8 chars) */
    for (int i = 0; i < 4; i++) {
        sprintf(uuid_out + (i * 2), "%02x", digest[i]);
    }
    uuid_out[PATH_UUID_LEN] = '\0';
}

/*
 * Check if a file is locked by another bead
 * Returns: 1 if locked (should block), 0 if not locked (can proceed)
 */
static int is_file_locked(const char *path) {
    char path_uuid[PATH_UUID_LEN + 1];
    DIR *dir;
    struct dirent *entry;
    int found_lock = 0;

    /* Compute path UUID */
    compute_path_uuid(path, path_uuid);
    DEBUG_LOG("Checking lock for: %s (uuid: %s)", path, path_uuid);

    /* Open lock directory */
    dir = opendir(lock_dir);
    if (!dir) {
        DEBUG_LOG("Lock directory %s does not exist, no locks", lock_dir);
        return 0;  /* No lock directory = no locks */
    }

    /* Look for lock files matching *-{path_uuid} */
    while ((entry = readdir(dir)) != NULL) {
        char *name = entry->d_name;
        size_t name_len = strlen(name);

        /* Check if this lock file matches our path UUID */
        if (name_len > PATH_UUID_LEN + 1) {
            char *suffix = name + (name_len - PATH_UUID_LEN);
            if (*suffix == *path_uuid && strcmp(suffix, path_uuid) == 0) {
                /* Found a matching lock file */
                char *dash = strrchr(name, '-');
                if (dash && (dash - name) > 0) {
                    /* Extract bead ID from lock filename: {bead-id}-{uuid} */
                    size_t bead_id_len = dash - name;
                    char lock_bead_id[64];

                    if (bead_id_len < sizeof(lock_bead_id)) {
                        strncpy(lock_bead_id, name, bead_id_len);
                        lock_bead_id[bead_id_len] = '\0';

                        /* If lock is held by our own bead, allow access */
                        if (current_bead_id[0] != '\0' &&
                            strcmp(lock_bead_id, current_bead_id) == 0) {
                            DEBUG_LOG("Lock held by current bead %s, allowing", lock_bead_id);
                            closedir(dir);
                            return 0;  /* Our own lock, allow */
                        }

                        DEBUG_LOG("File locked by bead %s", lock_bead_id);
                        found_lock = 1;
                        break;
                    }
                }
            }
        }
    }

    closedir(dir);
    return found_lock;
}

/*
 * Check if flags indicate a write operation
 */
static int is_write_operation(int flags) {
    return (flags & O_WRONLY) || (flags & O_RDWR) || (flags & O_CREAT) || (flags & O_TRUNC);
}

/*
 * Initialize the library
 */
static void init_library(void) {
    if (initialized) return;

    /* Get real function pointers */
    real_open = dlsym(RTLD_NEXT, "open");
    real_openat = dlsym(RTLD_NEXT, "openat");

    if (!real_open || !real_openat) {
        const char *err = dlerror();
        fprintf(stderr, "[libcheckout] ERROR: Failed to get real functions: %s\n", err ? err : "unknown");
        _exit(1);
    }

    /* Check for debug mode */
    debug_enabled = (getenv(DEBUG_ENV) != NULL && strcmp(getenv(DEBUG_ENV), "1") == 0);

    /* Get current bead ID (locks held by this bead are allowed) */
    const char *bead = getenv(BEAD_ID_ENV);
    if (bead) {
        strncpy(current_bead_id, bead, sizeof(current_bead_id) - 1);
        current_bead_id[sizeof(current_bead_id) - 1] = '\0';
        DEBUG_LOG("Current bead ID: %s", current_bead_id);
    }

    /* Get lock directory */
    const char *dir = getenv(LOCK_DIR_ENV);
    if (dir) {
        strncpy(lock_dir, dir, sizeof(lock_dir) - 1);
        lock_dir[sizeof(lock_dir) - 1] = '\0';
    }
    DEBUG_LOG("Lock directory: %s", lock_dir);

    initialized = 1;
    DEBUG_LOG("Library initialized");
}

/*
 * Intercepted open() function
 */
int open(const char *pathname, int flags, ...) {
    mode_t mode = 0;

    init_library();

    /* Extract mode if provided */
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, mode_t);
        va_end(args);
    }

    /* Check write operations against lock */
    if (is_write_operation(flags)) {
        if (is_file_locked(pathname)) {
            DEBUG_LOG("Blocking write to locked file: %s", pathname);
            errno = EACCES;
            return -1;
        }
    }

    /* Pass through to real open */
    return real_open(pathname, flags, mode);
}

/*
 * Intercepted openat() function
 */
int openat(int dirfd, const char *pathname, int flags, ...) {
    mode_t mode = 0;

    init_library();

    /* Extract mode if provided */
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, mode_t);
        va_end(args);
    }

    /* Check write operations against lock */
    if (is_write_operation(flags)) {
        if (is_file_locked(pathname)) {
            DEBUG_LOG("Blocking write to locked file: %s", pathname);
            errno = EACCES;
            return -1;
        }
    }

    /* Pass through to real openat */
    return real_openat(dirfd, pathname, flags, mode);
}

/*
 * Library version info (for debugging)
 */
const char *libcheckout_version(void) {
    return "libcheckout.so v1.0.0 - NEEDLE file lock enforcement";
}
