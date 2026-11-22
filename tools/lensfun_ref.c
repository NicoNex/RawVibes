/* Reference values from liblensfun 0.3.4, for RawVibes's port (core/src/lens.rs).
 * Dev only, not shipped:
 *   cc tools/lensfun_ref.c -llensfun -o target/lensfun_ref && target/lensfun_ref > core/tests/data/lensfun-ref.json
 * Each case: the interpolated calibration, the auto-scale, and for sample pixels of the
 * corrected image the sensor pixel each channel samples (distortion + TCA) and the
 * vignetting gain (on a white pixel). */
#include <lensfun/lensfun.h>
#include <stdio.h>
#include <string.h>

struct Case {
    const char *cam_maker, *cam_model, *lens_maker, *lens_model;
    float focal, aperture, distance;
};

static const struct Case CASES[] = {
    {"Canon", "Canon EOS 5D Mark III", "Canon", "Canon EF 16-35mm f/2.8L USM", 16, 4, 1000},
    {"Canon", "Canon EOS 5D Mark III", "Canon", "Canon EF 16-35mm f/2.8L USM", 19, 2.8, 1000},
    {"Canon", "Canon EOS 650D", "Canon", "Canon EF-S 10-22mm f/3.5-4.5 USM", 13, 5.6, 2},
    {"Canon", "Canon EOS 5D Mark III", "Canon", "Canon EF-S 10-22mm f/3.5-4.5 USM", 10, 4, 10},
    {"Canon", "Canon EOS M", "Canon", "Canon EF-M 22mm f/2 STM", 22, 2.8, 1000},
    {"Sony", "ILCE-6000", "Sony", "E 24mm f/1.8 ZA", 24, 1.8, 1000},
    {"Panasonic", "DC-S5", "Panasonic", "Lumix S 20-60/F3.5-5.6", 35, 6.3, 5},
    {"Canon", "Canon PowerShot G12", "Canon", "Canon PowerShot G12 & compatibles (Standard)", 12, 4, 1000},
};

static const char *dist_model(int m) {
    return m == LF_DIST_MODEL_POLY3 ? "poly3" : m == LF_DIST_MODEL_POLY5 ? "poly5" : m == LF_DIST_MODEL_PTLENS ? "ptlens" : "none";
}
static const char *tca_model(int m) {
    return m == LF_TCA_MODEL_LINEAR ? "linear" : m == LF_TCA_MODEL_POLY3 ? "poly3" : "none";
}

int main(void) {
    lfDatabase *db = lf_db_new();
    if (lf_db_load(db) != LF_NO_ERROR) return 1;
    printf("[\n");
    int first = 1;
    for (unsigned c = 0; c < sizeof CASES / sizeof *CASES; c++) {
        const struct Case *k = &CASES[c];
        const lfCamera **cams = lf_db_find_cameras(db, k->cam_maker, k->cam_model);
        const lfLens **lenses = lf_db_find_lenses_hd(db, NULL, k->lens_maker, k->lens_model, LF_SEARCH_LOOSE);
        if (!cams || !lenses) {
            fprintf(stderr, "not found: %s / %s\n", k->cam_model, k->lens_model);
            return 1;
        }
        const lfLens *lens = lenses[0];
        float crop = cams[0]->CropFactor;
        lfLensCalibDistortion d = {0};
        lfLensCalibTCA t = {0};
        lfLensCalibVignetting v = {0};
        lf_lens_interpolate_distortion(lens, k->focal, &d);
        lf_lens_interpolate_tca(lens, k->focal, &t);
        lf_lens_interpolate_vignetting(lens, k->focal, k->aperture, k->distance, &v);
        for (int portrait = 0; portrait < 2; portrait++) {
            int w = portrait ? 4000 : 6000, h = portrait ? 6000 : 4000;
            lfModifier *m = lf_modifier_new(lens, crop, w, h);
            int flags = lf_modifier_initialize(m, lens, LF_PF_F32, k->focal, k->aperture, k->distance, 1.0f,
                                               lens->Type, LF_MODIFY_TCA | LF_MODIFY_VIGNETTING | LF_MODIFY_DISTORTION, 0);
            float scale = lf_modifier_get_auto_scale(m, 0);
            printf("%s{\"camera\":\"%s\",\"lens\":\"%s\",\"camera_crop\":%g,\"w\":%d,\"h\":%d,\"focal\":%g,\"aperture\":%g,\"distance\":%g,"
                   "\"flags\":%d,\"dist\":{\"model\":\"%s\",\"k\":[%.9g,%.9g,%.9g]},"
                   "\"tca\":{\"model\":\"%s\",\"k\":[%.9g,%.9g,%.9g,%.9g,%.9g,%.9g]},\"vig\":[%.9g,%.9g,%.9g],"
                   "\"autoscale\":%.9g,\"points\":[",
                   first ? "" : ",\n", k->cam_model, k->lens_model, crop, w, h, k->focal, k->aperture, k->distance, flags,
                   dist_model(d.Model), d.Terms[0], d.Terms[1], d.Terms[2], tca_model(t.Model), t.Terms[0], t.Terms[1],
                   t.Terms[2], t.Terms[3], t.Terms[4], t.Terms[5], v.Terms[0], v.Terms[1], v.Terms[2], scale);
            first = 0;
            float xs[] = {0, w * 0.25f, w * 0.5f, w - 1.0f}, ys[] = {0, h * 0.5f, h - 1.0f, h * 0.8f};
            int n = 0;
            for (int i = 0; i < 4; i++)
                for (int j = 0; j < 4; j++) {
                    float res[6], px[3] = {1, 1, 1};
                    lf_modifier_apply_subpixel_geometry_distortion(m, xs[i], ys[j], 1, 1, res);
                    lf_modifier_apply_color_modification(m, px, xs[i], ys[j], 1, 1,
                                                         LF_CR_3(RED, GREEN, BLUE), 3 * sizeof(float));
                    printf("%s{\"p\":[%g,%g],\"src\":[%.6f,%.6f,%.6f,%.6f,%.6f,%.6f],\"gain\":%.7g}", n++ ? "," : "",
                           xs[i], ys[j], res[0], res[1], res[2], res[3], res[4], res[5], px[1]);
                }
            printf("]}");
            lf_modifier_destroy(m);
        }
        lf_free(cams);
        lf_free(lenses);
    }
    printf("\n]\n");
    lf_db_destroy(db);
    return 0;
}
