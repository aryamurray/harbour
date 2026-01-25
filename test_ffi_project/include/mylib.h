#ifndef MYLIB_H
#define MYLIB_H

#include <stdint.h>

// Constants
#define MYLIB_VERSION_MAJOR 1
#define MYLIB_VERSION_MINOR 0
#define MYLIB_MAX_SIZE 1024

// Enums
typedef enum {
    MYLIB_OK = 0,
    MYLIB_ERROR = 1,
    MYLIB_INVALID = 2
} mylib_status_t;

typedef enum {
    MYLIB_TYPE_INT = 0,
    MYLIB_TYPE_FLOAT = 1,
    MYLIB_TYPE_STRING = 2
} mylib_type_t;

// Structs
typedef struct {
    int32_t x;
    int32_t y;
} mylib_point_t;

typedef struct {
    char* name;
    int32_t id;
    mylib_type_t type;
} mylib_item_t;

// Functions
int mylib_init(void);
void mylib_shutdown(void);
int mylib_add(int a, int b);
mylib_status_t mylib_process(const char* input, char* output, size_t output_size);
mylib_point_t mylib_create_point(int32_t x, int32_t y);
void mylib_free_item(mylib_item_t* item);

#endif
