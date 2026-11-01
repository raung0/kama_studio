#import <Foundation/Foundation.h>
#import <IOSurface/IOSurface.h>
#import <Metal/Metal.h>
#include <bsm/libbsm.h>
#include <errno.h>
#include <mach/mach.h>
#include <servers/bootstrap.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define KAMA_CHANNEL_CAPACITY 192
#define KAMA_TOKEN_SIZE 32
#define KAMA_RGBA16FLOAT_FOURCC 0x52476841u
#define KAMA_MAX_DIMENSION 16384u

typedef struct {
    char channel[KAMA_CHANNEL_CAPACITY];
    uint8_t token[KAMA_TOKEN_SIZE];
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint64_t registry_id;
    int32_t importer_pid;
    int32_t exporter_pid;
} KamaSurfaceInfo;
_Static_assert(sizeof(KamaSurfaceInfo) == 256, "Rust/C shared-surface metadata ABI mismatch");

typedef struct {
    IOSurfaceRef surface;
    mach_port_t receive_port;
    pthread_t server_thread;
    _Atomic uint32_t references;
    _Atomic bool stopping;
    bool thread_started;
    char channel[KAMA_CHANNEL_CAPACITY];
    uint8_t token[KAMA_TOKEN_SIZE];
    uint32_t width;
    uint32_t height;
    int32_t importer_pid;
} KamaSurface;

typedef struct {
    mach_msg_header_t header;
    uint8_t token[KAMA_TOKEN_SIZE];
} KamaRequest;

typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t surface_port;
    uint8_t token[KAMA_TOKEN_SIZE];
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
} KamaResponse;

typedef union {
    max_align_t alignment;
    uint8_t bytes[sizeof(KamaRequest) + sizeof(mach_msg_audit_trailer_t) + 256];
} KamaReceiveBuffer;

static id<MTLTexture> make_texture(id<MTLDevice> device, IOSurfaceRef surface,
                                   uint32_t width, uint32_t height) {
    MTLTextureDescriptor *descriptor = [MTLTextureDescriptor
        texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA16Float
        width:width height:height mipmapped:NO];
    descriptor.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead |
                       MTLTextureUsageShaderWrite;
    descriptor.storageMode = MTLStorageModeShared;
    return [device newTextureWithDescriptor:descriptor iosurface:surface plane:0];
}

static int32_t message_peer_pid(void *message, size_t message_size, size_t capacity) {
    if (message_size < sizeof(mach_msg_header_t) || message_size > capacity) return -1;
    mach_msg_header_t *header = message;
    uintptr_t trailer_offset = (uintptr_t)header + ((message_size + sizeof(uint32_t) - 1) & ~(sizeof(uint32_t) - 1));
    if (trailer_offset > (uintptr_t)message + capacity ||
        sizeof(mach_msg_audit_trailer_t) > (uintptr_t)message + capacity - trailer_offset) return -1;
    mach_msg_audit_trailer_t *trailer = (mach_msg_audit_trailer_t *)trailer_offset;
    if (trailer->msgh_trailer_type != MACH_MSG_TRAILER_FORMAT_0 ||
        trailer->msgh_trailer_size < sizeof(*trailer)) {
        return -1;
    }
    return audit_token_to_pid(trailer->msgh_audit);
}

static void dispose_port(mach_port_t port, mach_port_right_t right) {
    if (port != MACH_PORT_NULL) {
        mach_port_mod_refs(mach_task_self(), port, right, -1);
    }
}

static void surface_release_ref(KamaSurface *surface) {
    if (atomic_fetch_sub_explicit(&surface->references, 1, memory_order_acq_rel) != 1) return;
    // Destroying the receive right first would turn our send right into a leaked dead-name right.
    dispose_port(surface->receive_port, MACH_PORT_RIGHT_SEND);
    dispose_port(surface->receive_port, MACH_PORT_RIGHT_RECEIVE);
    if (surface->surface) CFRelease(surface->surface);
    free(surface);
}

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
static kern_return_t register_channel(const char *channel, mach_port_t port) {
    return bootstrap_register(bootstrap_port, (char *)channel, port);
}
#pragma clang diagnostic pop

static void deregister_surface(KamaSurface *surface) {
    (void)register_channel(surface->channel, MACH_PORT_NULL);
}

static void *surface_server(void *opaque) {
    KamaSurface *surface = opaque;
    while (!atomic_load_explicit(&surface->stopping, memory_order_acquire)) {
        KamaReceiveBuffer buffer = {0};
        mach_msg_header_t *header = (mach_msg_header_t *)buffer.bytes;
        kern_return_t result = mach_msg(
            header,
            MACH_RCV_MSG | MACH_RCV_TIMEOUT |
                MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0) |
                MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT),
            0,
            sizeof(buffer.bytes),
            surface->receive_port,
            100,
            MACH_PORT_NULL);
        if (result == MACH_RCV_TIMED_OUT) continue;
        if (result != KERN_SUCCESS) break;
        if ((header->msgh_bits & MACH_MSGH_BITS_COMPLEX) ||
            header->msgh_size != sizeof(KamaRequest) ||
            message_peer_pid(buffer.bytes, header->msgh_size, sizeof(buffer.bytes)) != surface->importer_pid) {
            mach_msg_destroy(header);
            continue;
        }
        KamaRequest *request = (KamaRequest *)buffer.bytes;
        if (memcmp(request->token, surface->token, KAMA_TOKEN_SIZE) != 0 ||
            header->msgh_remote_port == MACH_PORT_NULL) {
            mach_msg_destroy(header);
            continue;
        }

        KamaResponse response = {0};
        response.header.msgh_bits = MACH_MSGH_BITS_COMPLEX |
            MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0);
        response.header.msgh_size = sizeof(response);
        response.header.msgh_remote_port = header->msgh_remote_port;
        header->msgh_remote_port = MACH_PORT_NULL;
        response.body.msgh_descriptor_count = 1;
        response.surface_port.name = IOSurfaceCreateMachPort(surface->surface);
        response.surface_port.disposition = MACH_MSG_TYPE_MOVE_SEND;
        response.surface_port.type = MACH_MSG_PORT_DESCRIPTOR;
        memcpy(response.token, surface->token, KAMA_TOKEN_SIZE);
        response.width = surface->width;
        response.height = surface->height;
        response.pixel_format = KAMA_RGBA16FLOAT_FOURCC;
        if (response.surface_port.name == MACH_PORT_NULL) {
            dispose_port(response.header.msgh_remote_port, MACH_PORT_RIGHT_SEND_ONCE);
            continue;
        }
        result = mach_msg(&response.header, MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                          response.header.msgh_size, 0, MACH_PORT_NULL, 250,
                          MACH_PORT_NULL);
        if (result != KERN_SUCCESS && response.surface_port.name != MACH_PORT_NULL) {
            mach_port_deallocate(mach_task_self(), response.surface_port.name);
        }
        if (result != KERN_SUCCESS && response.header.msgh_remote_port != MACH_PORT_NULL) {
            mach_port_deallocate(mach_task_self(), response.header.msgh_remote_port);
        }
    }
    if (!atomic_load_explicit(&surface->stopping, memory_order_acquire)) {
        deregister_surface(surface);
    }
    surface_release_ref(surface);
    return NULL;
}

int32_t kama_shared_surface_create(void *metal_device, uint32_t width, uint32_t height,
                                   int32_t peer_pid, KamaSurfaceInfo *info,
                                   void **surface_out, void **texture_out) {
    if (!metal_device || !info || !surface_out || !texture_out || width == 0 || height == 0 ||
        width > KAMA_MAX_DIMENSION || height > KAMA_MAX_DIMENSION || peer_pid <= 1) return EINVAL;
    @autoreleasepool {
        id<MTLDevice> device = (__bridge id<MTLDevice>)metal_device;
        size_t row_bytes = IOSurfaceAlignProperty(kIOSurfaceBytesPerRow, (size_t)width * 8);
        if (row_bytes == 0 || height > SIZE_MAX / row_bytes) return EOVERFLOW;
        NSDictionary *properties = @{
            (__bridge NSString *)kIOSurfaceWidth: @(width),
            (__bridge NSString *)kIOSurfaceHeight: @(height),
            (__bridge NSString *)kIOSurfaceBytesPerElement: @8,
            (__bridge NSString *)kIOSurfaceBytesPerRow: @(row_bytes),
            (__bridge NSString *)kIOSurfaceAllocSize: @(row_bytes * height),
            (__bridge NSString *)kIOSurfacePixelFormat: @(KAMA_RGBA16FLOAT_FOURCC),
        };
        IOSurfaceRef iosurface = IOSurfaceCreate((__bridge CFDictionaryRef)properties);
        if (!iosurface) return ENOMEM;
        id<MTLTexture> texture = make_texture(device, iosurface, width, height);
        if (!texture) { CFRelease(iosurface); return ENOTSUP; }

        KamaSurface *surface = calloc(1, sizeof(*surface));
        if (!surface) { CFRelease(iosurface); return ENOMEM; }
        surface->surface = iosurface;
        surface->width = width;
        surface->height = height;
        surface->importer_pid = peer_pid;
        arc4random_buf(surface->token, sizeof(surface->token));
        char token_hex[KAMA_TOKEN_SIZE * 2 + 1];
        for (size_t i = 0; i < sizeof(surface->token); ++i) {
            snprintf(token_hex + i * 2, 3, "%02x", surface->token[i]);
        }
        int written = snprintf(surface->channel, sizeof(surface->channel),
                               "app.kama.surface.%d.%s", getpid(), token_hex);
        if (written <= 0 || (size_t)written >= sizeof(surface->channel)) {
            CFRelease(iosurface); free(surface); return ENAMETOOLONG;
        }
        kern_return_t result = mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE,
                                                   &surface->receive_port);
        if (result == KERN_SUCCESS) {
            result = mach_port_insert_right(mach_task_self(), surface->receive_port,
                                            surface->receive_port, MACH_MSG_TYPE_MAKE_SEND);
        }
        if (result == KERN_SUCCESS) {
            result = register_channel(surface->channel, surface->receive_port);
        }
        if (result != KERN_SUCCESS) {
            if (surface->receive_port != MACH_PORT_NULL) {
                dispose_port(surface->receive_port, MACH_PORT_RIGHT_SEND);
                dispose_port(surface->receive_port, MACH_PORT_RIGHT_RECEIVE);
            }
            CFRelease(iosurface); free(surface); return result;
        }
        atomic_init(&surface->stopping, false);
        atomic_init(&surface->references, 2);
        pthread_attr_t attributes;
        int thread_error = pthread_attr_init(&attributes);
        if (thread_error == 0) {
            // The server owns a context reference and releases it on exit; no UI thread joins it.
            thread_error = pthread_attr_setdetachstate(&attributes, PTHREAD_CREATE_DETACHED);
            if (thread_error == 0) {
                thread_error = pthread_create(&surface->server_thread, &attributes, surface_server, surface);
            }
            pthread_attr_destroy(&attributes);
        }
        if (thread_error != 0) {
            atomic_store(&surface->references, 1);
            deregister_surface(surface);
            surface_release_ref(surface);
            return thread_error;
        }
        surface->thread_started = true;
        memset(info, 0, sizeof(*info));
        memcpy(info->channel, surface->channel, strlen(surface->channel) + 1);
        memcpy(info->token, surface->token, sizeof(info->token));
        info->width = width;
        info->height = height;
        info->pixel_format = KAMA_RGBA16FLOAT_FOURCC;
        info->registry_id = device.registryID;
        info->importer_pid = peer_pid;
        info->exporter_pid = getpid();
        *surface_out = surface;
        *texture_out = (void *)CFBridgingRetain(texture);
        return 0;
    }
}

int32_t kama_shared_surface_receive(void *metal_device, const KamaSurfaceInfo *info,
                                    uint32_t timeout_ms, void **surface_out,
                                    void **texture_out) {
    if (!metal_device || !info || !surface_out || !texture_out ||
        info->width == 0 || info->height == 0 || info->width > KAMA_MAX_DIMENSION ||
        info->height > KAMA_MAX_DIMENSION || info->pixel_format != KAMA_RGBA16FLOAT_FOURCC ||
        info->importer_pid <= 1 || info->exporter_pid <= 1 || info->importer_pid != getpid() ||
        timeout_ms == 0 || timeout_ms > 2000) return EINVAL;
    @autoreleasepool {
        id<MTLDevice> device = (__bridge id<MTLDevice>)metal_device;
        if (device.registryID != info->registry_id) return ENODEV;
        mach_port_t server = MACH_PORT_NULL;
        kern_return_t result = bootstrap_look_up(bootstrap_port, info->channel, &server);
        if (result != KERN_SUCCESS) return result;
        mach_port_t reply = MACH_PORT_NULL;
        result = mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &reply);
        if (result != KERN_SUCCESS) { mach_port_deallocate(mach_task_self(), server); return result; }

        KamaRequest request = {0};
        request.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND_ONCE);
        request.header.msgh_size = sizeof(request);
        request.header.msgh_remote_port = server;
        request.header.msgh_local_port = reply;
        memcpy(request.token, info->token, sizeof(request.token));
        result = mach_msg(&request.header, MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                          request.header.msgh_size, 0, MACH_PORT_NULL,
                          timeout_ms, MACH_PORT_NULL);
        mach_port_deallocate(mach_task_self(), server);
        if (result != KERN_SUCCESS) {
            dispose_port(reply, MACH_PORT_RIGHT_SEND_ONCE);
            dispose_port(reply, MACH_PORT_RIGHT_RECEIVE);
            return result;
        }

        union {
            max_align_t alignment;
            uint8_t bytes[sizeof(KamaResponse) + sizeof(mach_msg_audit_trailer_t) + 256];
        } received = {0};
        result = mach_msg((mach_msg_header_t *)received.bytes,
                          MACH_RCV_MSG | MACH_RCV_TIMEOUT |
                              MACH_RCV_TRAILER_TYPE(MACH_MSG_TRAILER_FORMAT_0) |
                              MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT),
                          0, sizeof(received.bytes), reply, timeout_ms, MACH_PORT_NULL);
        dispose_port(reply, MACH_PORT_RIGHT_RECEIVE);
        if (result != KERN_SUCCESS) return result;
        KamaResponse *response = (KamaResponse *)received.bytes;
        if (response->header.msgh_size != sizeof(KamaResponse) ||
            response->header.msgh_size > sizeof(received.bytes) - sizeof(mach_msg_audit_trailer_t) ||
            message_peer_pid(received.bytes, response->header.msgh_size, sizeof(received.bytes)) != info->exporter_pid ||
            !(response->header.msgh_bits & MACH_MSGH_BITS_COMPLEX) ||
            response->body.msgh_descriptor_count != 1 ||
            response->width != info->width || response->height != info->height ||
            response->pixel_format != info->pixel_format ||
            memcmp(response->token, info->token, KAMA_TOKEN_SIZE) != 0 ||
            response->surface_port.type != MACH_MSG_PORT_DESCRIPTOR) {
            mach_msg_destroy(&response->header);
            return EPROTO;
        }
        IOSurfaceRef iosurface = IOSurfaceLookupFromMachPort(response->surface_port.name);
        mach_port_deallocate(mach_task_self(), response->surface_port.name);
        if (!iosurface) return ENOENT;
        if (IOSurfaceGetWidth(iosurface) != info->width ||
            IOSurfaceGetHeight(iosurface) != info->height ||
            IOSurfaceGetPixelFormat(iosurface) != info->pixel_format ||
            IOSurfaceGetBytesPerElement(iosurface) != 8 ||
            IOSurfaceGetPlaneCount(iosurface) != 0) {
            CFRelease(iosurface); return EINVAL;
        }
        id<MTLTexture> texture = make_texture(device, iosurface, info->width, info->height);
        if (!texture) { CFRelease(iosurface); return ENOTSUP; }
        KamaSurface *surface = calloc(1, sizeof(*surface));
        if (!surface) { CFRelease(iosurface); return ENOMEM; }
        surface->surface = iosurface;
        surface->width = info->width;
        surface->height = info->height;
        atomic_init(&surface->references, 1);
        *surface_out = surface;
        *texture_out = (void *)CFBridgingRetain(texture);
        return 0;
    }
}

void kama_shared_surface_release(void *opaque) {
    KamaSurface *surface = opaque;
    if (!surface) return;
    if (surface->thread_started) {
        atomic_store_explicit(&surface->stopping, true, memory_order_release);
        deregister_surface(surface);
        surface_release_ref(surface);
        return;
    }
    surface_release_ref(surface);
}

void kama_shared_surface_release_texture(void *texture) {
    if (texture) CFRelease((CFTypeRef)texture);
}
