/* Exercise cJSON at runtime, not just at link time.
 *
 * Deliberately touches *both* translation units: cJSONUtils_GetPointer lives
 * in cJSON_Utils.c, so this fails to link if only cJSON.c were listed in the
 * shim -- which is the failure a build-only check cannot see, because a
 * static archive missing a member still archives successfully.
 *
 * The float assertions are what exercise `system_libs = ["m"]` on cJSON's
 * *public* surface: the number printer calls floor/fabs, and an archive does
 * not record that dependency. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cJSON.h"
#include "cJSON_Utils.h"

static int failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            printf("FAIL line %d: %s\n", __LINE__, #cond);                     \
            failures++;                                                        \
        }                                                                      \
    } while (0)

int main(void) {
    const char *doc = "{\"name\":\"harbour\",\"n\":42,\"f\":3.5,"
                      "\"arr\":[1,2,3],\"nested\":{\"k\":\"v\"}}";

    cJSON *root = cJSON_Parse(doc);
    if (root == NULL) {
        printf("FAIL: cJSON_Parse returned NULL\n");
        return 1;
    }

    cJSON *name = cJSON_GetObjectItemCaseSensitive(root, "name");
    CHECK(cJSON_IsString(name));
    CHECK(name != NULL && strcmp(name->valuestring, "harbour") == 0);

    cJSON *n = cJSON_GetObjectItemCaseSensitive(root, "n");
    CHECK(cJSON_IsNumber(n));
    CHECK(n != NULL && n->valueint == 42);

    /* libm path. */
    cJSON *f = cJSON_GetObjectItemCaseSensitive(root, "f");
    CHECK(cJSON_IsNumber(f));
    CHECK(f != NULL && f->valuedouble > 3.49 && f->valuedouble < 3.51);

    CHECK(cJSON_GetArraySize(cJSON_GetObjectItemCaseSensitive(root, "arr")) == 3);

    /* Second translation unit. */
    cJSON *deep = cJSONUtils_GetPointer(root, "/nested/k");
    CHECK(deep != NULL);
    CHECK(cJSON_IsString(deep));
    CHECK(deep != NULL && strcmp(deep->valuestring, "v") == 0);

    /* Round trip through the printer, which is the other libm user. */
    char *printed = cJSON_PrintUnformatted(root);
    CHECK(printed != NULL);
    if (printed != NULL) {
        cJSON *again = cJSON_Parse(printed);
        CHECK(again != NULL);
        CHECK(cJSON_Compare(root, again, 1));
        cJSON_Delete(again);
        free(printed);
    }

    cJSON_Delete(root);

    if (failures != 0) {
        printf("%d check(s) failed\n", failures);
        return 1;
    }
    printf("OK cjson %s: parse, both translation units, libm, round trip\n",
           cJSON_Version());
    return 0;
}
