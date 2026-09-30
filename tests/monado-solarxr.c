// Compatibility oracle: compile with WiVRn's actual Monado solarxr/protocol.c.
#include "protocol.h"
#include <math.h>
#include <stdlib.h>

int main(void)
{
    const unsigned char ids[] = {3, 8, 9};
    const enum solarxr_body_part roles[] = {
        SOLARXR_BODY_PART_WAIST, SOLARXR_BODY_PART_LEFT_FOOT, SOLARXR_BODY_PART_RIGHT_FOOT};
    for (unsigned frame = 0; frame < 3; ++frame) {
        unsigned char length[4];
        assert(fread(length, 1, 4, stdin) == 4);
        const size_t size = (size_t)length[0] | (size_t)length[1] << 8 |
                            (size_t)length[2] << 16 | (size_t)length[3] << 24;
        assert(size >= 8 && size < 0x100000);
        unsigned char *data = malloc(size - 4);
        assert(data != NULL && fread(data, 1, size - 4, stdin) == size - 4);
        struct solarxr_message_bundle bundle;
        assert(read_solarxr_message_bundle(&bundle, data, size - 4, (const void *)data));
        assert(bundle.data_feed_msgs.length == 1);
        struct solarxr_data_feed_message_header feed;
        assert(read_solarxr_data_feed_message_header(&feed, data, size - 4, bundle.data_feed_msgs.data));
        assert(feed.message_type == SOLARXR_DATA_FEED_MESSAGE_DATA_FEED_UPDATE);
        assert(feed.message.data_feed_update.synthetic_trackers.length == 3);
        for (unsigned i = 0; i < 3; ++i) {
            struct solarxr_tracker_data tracker;
            assert(read_solarxr_tracker_data(&tracker, data, size - 4,
                   &feed.message.data_feed_update.synthetic_trackers.data[i]));
            assert(!tracker.tracker_id.has_device_id && tracker.tracker_id.tracker_num == ids[i]);
            assert(tracker.has_info == (frame == 0));
            if (frame == 0) {
                assert(tracker.info.body_part == roles[i]);
                assert(tracker.info.display_name.length > 0);
            }
            if (frame == 1 && i == 0) {
                assert(tracker.has_rotation && tracker.has_position && !tracker.has_raw_angular_velocity);
                assert(fabsf(tracker.rotation.w - 0.70710678f) < 1e-6f);
                assert(fabsf(tracker.rotation.y - 0.70710678f) < 1e-6f);
                assert(tracker.position.x == 3.5f && tracker.position.y == 2 && tracker.position.z == -1.5f);
            } else {
                // A velocity-only sample makes Monado push an invalid pose into history.
                assert(!tracker.has_rotation && !tracker.has_position && tracker.has_raw_angular_velocity);
            }
        }
        assert(bundle.rpc_msgs.length == (frame < 2 ? 1u : 0u));
        if (frame < 2) {
            struct solarxr_rpc_message_header rpc;
            assert(read_solarxr_rpc_message_header(&rpc, data, size - 4, bundle.rpc_msgs.data));
            assert(rpc.message_type == SOLARXR_RPC_MESSAGE_TYPE_SETTINGS_RESPONSE);
            const struct solarxr_steamvr_trackers_setting s = rpc.message.settings_response.steam_vr_trackers;
            assert(s.waist && s.chest && s.left_foot && s.right_foot && s.left_knee && s.right_knee);
            assert(s.left_elbow && s.right_elbow && s.left_hand && s.right_hand);
        }
        free(data);
    }
    assert(fgetc(stdin) == EOF);
    return 0;
}
