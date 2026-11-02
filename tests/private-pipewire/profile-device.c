// SPDX-License-Identifier: MPL-2.0
/* A private-server-only native SPA Device. No ALSA, Bluetooth or host devices. */
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <pipewire/impl.h>
#include <spa/monitor/device.h>
#include <spa/monitor/utils.h>
#include <spa/param/param.h>
#include <spa/param/profile.h>
#include <spa/pod/builder.h>
#include <spa/pod/filter.h>
#include <spa/pod/parser.h>

struct fixture {
    struct spa_device device;
    struct spa_hook_list listeners;
    struct spa_param_info params[2];
    struct pw_main_loop *loop;
    struct pw_context *context;
    struct pw_impl_device *impl_device;
    struct pw_impl_node *input;
    struct pw_impl_node *output;
    int32_t profile;
};

static const char *names[] = { "off", "duplex", "unavailable", "reject", "stall" };
static const char *descriptions[] = {
    "Off (both endpoints removed)", "Synthetic input and output", "Unavailable profile",
    "Native EIO rejection", "No confirmation (timeout fixture)"
};

static void emit_info(struct fixture *f)
{
    const struct spa_dict_item items[] = {
        { PW_KEY_DEVICE_NAME, "cosmos.profile.fixture" },
        { PW_KEY_DEVICE_DESCRIPTION, "Synthetic profile device" },
        { PW_KEY_MEDIA_CLASS, "Audio/Device" },
        { "device.api", "cosmos-test" },
    };
    const struct spa_dict props = SPA_DICT_INIT_ARRAY(items);
    const struct spa_device_info info = {
        .version = SPA_VERSION_DEVICE_INFO,
        .change_mask = SPA_DEVICE_CHANGE_MASK_PROPS | SPA_DEVICE_CHANGE_MASK_PARAMS,
        .props = &props,
        .params = f->params,
        .n_params = 2,
    };
    spa_device_emit_info(&f->listeners, &info);
}

static int add_listener(void *object, struct spa_hook *hook,
                        const struct spa_device_events *events, void *data)
{
    struct fixture *f = object;
    struct spa_hook_list saved;
    spa_hook_list_isolate(&f->listeners, &saved, hook, events, data);
    emit_info(f);
    spa_hook_list_join(&f->listeners, &saved);
    return 0;
}

static int sync_device(void *object, int seq)
{
    struct fixture *f = object;
    spa_device_emit_result(&f->listeners, seq, 0, 0, NULL);
    return 0;
}

static int enum_params(void *object, int seq, uint32_t id, uint32_t start,
                       uint32_t max, const struct spa_pod *filter)
{
    struct fixture *f = object;
    if (id != SPA_PARAM_EnumProfile && id != SPA_PARAM_Profile)
        return -ENOENT;
    uint32_t count = id == SPA_PARAM_EnumProfile ? SPA_N_ELEMENTS(names) : 1;
    for (uint32_t index = start, emitted = 0; index < count && emitted < max; index++) {
        int32_t selected = id == SPA_PARAM_Profile ? f->profile : (int32_t)index;
        uint8_t buffer[1024], filtered[1024];
        struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
        struct spa_pod_builder fb = SPA_POD_BUILDER_INIT(filtered, sizeof(filtered));
        struct spa_pod *pod = spa_pod_builder_add_object(&builder,
            SPA_TYPE_OBJECT_ParamProfile, id,
            SPA_PARAM_PROFILE_index, SPA_POD_Int(selected),
            SPA_PARAM_PROFILE_name, SPA_POD_String(names[selected]),
            SPA_PARAM_PROFILE_description, SPA_POD_String(descriptions[selected]),
            SPA_PARAM_PROFILE_available, SPA_POD_Id(selected == 2 ? SPA_PARAM_AVAILABILITY_no : SPA_PARAM_AVAILABILITY_yes));
        if (spa_pod_filter(&fb, &pod, pod, filter) < 0)
            continue;
        const struct spa_result_device_params result = {
            .id = id, .index = index, .next = index + 1, .param = pod,
        };
        spa_device_emit_result(&f->listeners, seq, 0, SPA_RESULT_TYPE_DEVICE_PARAMS, &result);
        emitted++;
    }
    return 0;
}

static struct pw_impl_node *create_endpoint(struct fixture *f, const char *name, const char *class)
{
    struct pw_impl_factory *factory = pw_context_find_factory(f->context, "adapter");
    if (factory == NULL)
        return NULL;
    struct pw_properties *props = pw_properties_new(
        "factory.name", "support.null-audio-sink",
        PW_KEY_NODE_NAME, name,
        PW_KEY_NODE_DESCRIPTION, "Synthetic profile endpoint",
        PW_KEY_MEDIA_CLASS, class,
        "audio.channels", "1", "audio.position", "[ MONO ]",
        "node.virtual", "true", NULL);
    if (props == NULL)
        return NULL;
    pw_properties_setf(props, PW_KEY_DEVICE_ID, "%u",
                      pw_global_get_id(pw_impl_device_get_global(f->impl_device)));
    return pw_impl_factory_create_object(factory, NULL, PW_TYPE_INTERFACE_Node,
                                         PW_VERSION_NODE, props, SPA_ID_INVALID);
}

static int set_profile(void *object, uint32_t id, uint32_t flags, const struct spa_pod *pod)
{
    struct fixture *f = object;
    int32_t selected = -1;
    bool save = false;
    (void)flags;
    if (id != SPA_PARAM_Profile || pod == NULL)
        return -EINVAL;
    if (spa_pod_parse_object(pod, SPA_TYPE_OBJECT_ParamProfile, NULL,
        SPA_PARAM_PROFILE_index, SPA_POD_Int(&selected),
        SPA_PARAM_PROFILE_save, SPA_POD_Bool(&save)) < 0 || !save)
        return -EINVAL;
    if (selected < 0 || selected >= (int32_t)SPA_N_ELEMENTS(names) || selected == 2)
        return -EINVAL;
    if (selected == 3)
        return -EIO;
    if (selected == 4)
        return 0; /* Deliberate accepted-but-never-applied native request. */
    if (selected == 0) {
        if (f->input != NULL) { pw_impl_node_destroy(f->input); f->input = NULL; }
        if (f->output != NULL) { pw_impl_node_destroy(f->output); f->output = NULL; }
    } else {
        if (f->input == NULL) f->input = create_endpoint(f, "cosmos.profile.input", "Audio/Source");
        if (f->output == NULL) f->output = create_endpoint(f, "cosmos.profile.output", "Audio/Sink");
        if (f->input == NULL || f->output == NULL)
            return -ENOMEM;
    }
    f->profile = selected;
    f->params[1].flags ^= SPA_PARAM_INFO_SERIAL;
    emit_info(f);
    return 0;
}

static const struct spa_device_methods methods = {
    .version = SPA_VERSION_DEVICE_METHODS,
    .add_listener = add_listener,
    .sync = sync_device,
    .enum_params = enum_params,
    .set_param = set_profile,
};

static void remove_device(struct fixture *f)
{
    if (f->input != NULL) { pw_impl_node_destroy(f->input); f->input = NULL; }
    if (f->output != NULL) { pw_impl_node_destroy(f->output); f->output = NULL; }
    if (f->impl_device != NULL) {
        pw_impl_device_destroy(f->impl_device);
        f->impl_device = NULL;
    }
}

static int create_device(struct fixture *f)
{
    f->profile = 0;
    f->impl_device = pw_context_create_device(f->context, pw_properties_new(
        PW_KEY_DEVICE_NAME, "cosmos.profile.fixture",
        PW_KEY_DEVICE_DESCRIPTION, "Synthetic profile device",
        PW_KEY_MEDIA_CLASS, "Audio/Device", "device.api", "cosmos-test", NULL), 0);
    if (f->impl_device == NULL)
        return -errno;
    int result = pw_impl_device_set_implementation(f->impl_device, &f->device);
    if (result >= 0)
        result = pw_impl_device_register(f->impl_device, NULL);
    if (result < 0)
        remove_device(f);
    return result;
}

static void change_device_lifetime(void *data, int signal_number)
{
    struct fixture *f = data;
    if (signal_number == SIGUSR1)
        remove_device(f);
    else if (f->impl_device == NULL && create_device(f) < 0)
        pw_main_loop_quit(f->loop);
}

static void stop(void *data, int signal_number)
{
    (void)signal_number;
    pw_main_loop_quit(((struct fixture *)data)->loop);
}

int main(int argc, char **argv)
{
    /* Explicit private-server arguments prevent accidental host discovery. */
    if (argc != 2 || strcmp(argv[1], "cosmos-test") != 0 ||
        getenv("PIPEWIRE_RUNTIME_DIR") == NULL) {
        fprintf(stderr, "usage: profile-device cosmos-test (private PIPEWIRE_RUNTIME_DIR required)\n");
        return 2;
    }
    /* Block before pw_init/context can create workers. They must inherit this
     * mask so process-directed signals reach the main loop's signalfd, not an
     * unblocked PipeWire worker with the default terminating action. */
    sigset_t signals;
    if (sigemptyset(&signals) < 0 ||
        sigaddset(&signals, SIGINT) < 0 || sigaddset(&signals, SIGTERM) < 0 ||
        sigaddset(&signals, SIGUSR1) < 0 || sigaddset(&signals, SIGUSR2) < 0 ||
        sigprocmask(SIG_BLOCK, &signals, NULL) < 0) {
        perror("block fixture signals");
        return 1;
    }
    pw_init(&argc, &argv);
    struct fixture f = {0};
    spa_hook_list_init(&f.listeners);
    f.params[0] = (struct spa_param_info) { .id = SPA_PARAM_EnumProfile, .flags = SPA_PARAM_INFO_READ };
    f.params[1] = (struct spa_param_info) { .id = SPA_PARAM_Profile, .flags = SPA_PARAM_INFO_READWRITE };
    f.device.iface = SPA_INTERFACE_INIT(SPA_TYPE_INTERFACE_Device, SPA_VERSION_DEVICE, &methods, &f);
    f.loop = pw_main_loop_new(NULL);
    if (f.loop == NULL) return 1;
    f.context = pw_context_new(pw_main_loop_get_loop(f.loop),
                              pw_properties_new(PW_KEY_CONFIG_NAME, "daemon.conf", NULL), 0);
    if (f.context == NULL) return 1;
    if (create_device(&f) < 0) {
        fprintf(stderr, "native Device registration failed: %s\n", strerror(errno));
        return 1;
    }
    if (pw_loop_add_signal(pw_main_loop_get_loop(f.loop), SIGINT, stop, &f) == NULL ||
        pw_loop_add_signal(pw_main_loop_get_loop(f.loop), SIGTERM, stop, &f) == NULL ||
        pw_loop_add_signal(pw_main_loop_get_loop(f.loop), SIGUSR1, change_device_lifetime, &f) == NULL ||
        pw_loop_add_signal(pw_main_loop_get_loop(f.loop), SIGUSR2, change_device_lifetime, &f) == NULL) {
        perror("register fixture signal handler");
        remove_device(&f);
        pw_context_destroy(f.context);
        pw_main_loop_destroy(f.loop);
        pw_deinit();
        return 1;
    }
    pw_main_loop_run(f.loop);
    remove_device(&f);
    pw_context_destroy(f.context);
    pw_main_loop_destroy(f.loop);
    pw_deinit();
    return 0;
}
