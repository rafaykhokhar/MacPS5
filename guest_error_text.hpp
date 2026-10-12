#pragma once
#include <string>
#include <utility>
#include <cstdint>
namespace NativePS5 {
// Initial C-locale descriptions for the guest error numbers our services use.
// Never interpret a guest errno by indexing the host's errno table.
inline std::pair<std::string,bool> GuestErrorText(int32_t number){
    switch(number){
        case 0:return {"No error",true};
        case 1:return {"Operation not permitted",true};
        case 2:return {"No such file or directory",true};
        case 3:return {"No such process",true};
        case 4:return {"Interrupted system call",true};
        case 5:return {"Input/output error",true};
        case 9:return {"Bad file descriptor",true};
        case 12:return {"Cannot allocate memory",true};
        case 13:return {"Permission denied",true};
        case 14:return {"Bad address",true};
        case 22:return {"Invalid argument",true};
        case 32:return {"Broken pipe",true};
        case 35:return {"Resource temporarily unavailable",true};
        case 38:return {"Socket operation on non-socket",true};
        case 42:return {"Protocol not available",true};
        case 78:return {"Function not implemented",true};
        default:return {"Unknown error: "+std::to_string(number),false};
    }
}
}
