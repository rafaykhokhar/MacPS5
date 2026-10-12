#pragma once
#include "guest_cpu.hpp"
#include <algorithm>
#include <functional>
#include <stdexcept>
#include <string>

namespace NativePS5 {
class FormatUnsupported:public std::runtime_error {public:using std::runtime_error::runtime_error;};
inline uint64_t FormatAddress(uint64_t address,uint64_t offset){
    if(address>UINT64_MAX-offset)throw std::runtime_error("Format pointer overflow");
    return address+offset;
}
inline std::string FormatString(GuestMemory& memory,uint64_t address,size_t limit,bool bounded=false){
    std::string result;
    for(size_t i=0;i<limit;++i){auto c=memory.Load(FormatAddress(address,i),1);if(!c)return result;result.push_back(static_cast<char>(c));}
    if(!bounded)throw FormatUnsupported("Unterminated or oversized format string");
    return result;
}
// Integer/pointer arguments use the guest SysV register sequence and guest stack.
// Construct at function entry, after its return address has been pushed.
class FormatArguments {
    GuestCpu& cpu;
    unsigned index=3;
    uint64_t stack;
public:
    explicit FormatArguments(GuestCpu& cpu):cpu(cpu),stack(FormatAddress(cpu.registers[4],8)){}
    uint64_t Next(){
        constexpr unsigned registers[]={7,6,2,1,8,9};
        if(index<6)return cpu.registers[registers[index++]];
        const auto value=cpu.memory->Load(stack,8);stack=FormatAddress(stack,8);return value;
    }
};
// Copy the SysV va_list cursor. The caller's va_list contents are left intact.
// Floating-point slots are not consumed by the supported integer/string subset.
class FormatVaArguments {
    GuestMemory& memory;
    uint64_t offset,overflow,save;
public:
    FormatVaArguments(GuestMemory& memory,uint64_t address):memory(memory){
        memory.ValidateAccess(address,24);
        offset=memory.Load(address,4);
        overflow=memory.Load(FormatAddress(address,8),8);
        save=memory.Load(FormatAddress(address,16),8);
        if(offset>48 || offset%8)throw FormatUnsupported("Invalid SysV general-purpose argument offset");
    }
    uint64_t Next(){
        if(offset<48){const auto value=memory.Load(FormatAddress(save,offset),8);offset+=8;return value;}
        const auto value=memory.Load(overflow,8);overflow=FormatAddress(overflow,8);return value;
    }
};
// Build before writing: unsupported formats and inaccessible sources leave the
// destination unchanged. This bounded subset does not implement floating point,
// locale/wide strings, positional arguments or %n.
inline int GuestSnprintf(GuestMemory& memory,uint64_t destination,uint64_t capacity,uint64_t format,
                         const std::function<uint64_t()>& next){
    constexpr size_t Limit=65536;
    const auto text=FormatString(memory,format,Limit);
    std::string output;
    auto append=[&](std::string_view value){if(value.size()>Limit-output.size())throw FormatUnsupported("Formatted output limit");output+=value;};
    for(size_t i=0;i<text.size();){
        if(text[i]!='%'){append(std::string_view(text).substr(i++,1));continue;}
        ++i;if(i<text.size() && text[i]=='%'){append("%");++i;continue;}
        bool left=false,plus=false,space=false,alternate=false,zero=false;
        for(;i<text.size();++i){const char c=text[i];if(c=='-')left=true;else if(c=='+')plus=true;else if(c==' ')space=true;else if(c=='#')alternate=true;else if(c=='0')zero=true;else break;}
        auto number=[&](){size_t value=0;while(i<text.size() && text[i]>='0' && text[i]<='9'){
            value=value*10+size_t(text[i++]-'0');if(value>Limit)throw FormatUnsupported("Format width/precision limit");}return value;};
        size_t width=0;int precision=-1;
        if(i<text.size() && text[i]=='*'){
            ++i;int64_t value=static_cast<int32_t>(next());if(value<0){left=true;value=-value;}
            if(value>int64_t(Limit))throw FormatUnsupported("Format width limit");width=size_t(value);
        }else width=number();
        if(i<text.size() && text[i]=='.'){
            ++i;if(i<text.size() && text[i]=='*'){++i;precision=static_cast<int32_t>(next());if(precision<0)precision=-1;}
            else precision=static_cast<int>(number());
            if(precision>int(Limit))throw FormatUnsupported("Format precision limit");
        }
        unsigned bits=32;bool length=false;
        if(i<text.size() && text[i]=='h'){length=true;++i;bits=16;if(i<text.size() && text[i]=='h'){bits=8;++i;}}
        else if(i<text.size() && (text[i]=='l' || text[i]=='j' || text[i]=='z' || text[i]=='t')){
            length=true;bits=64;const auto c=text[i++];if(c=='l' && i<text.size() && text[i]=='l')++i;
        }
        if(i==text.size())throw FormatUnsupported("Incomplete format conversion");
        const char conversion=text[i++];std::string digits,prefix;
        bool numeric=false;
        if(conversion=='s'){
            if(length)throw FormatUnsupported("Wide string conversion unsupported");
            digits=FormatString(memory,next(),precision<0?Limit:size_t(precision),precision>=0);
        }else if(conversion=='c'){
            if(length)throw FormatUnsupported("Wide character conversion unsupported");
            digits.push_back(static_cast<char>(next()&255));
        }else if(std::string_view("diuoxXp").find(conversion)!=std::string_view::npos){
            numeric=true;if(conversion=='p'){if(length)throw FormatUnsupported("Pointer length modifier");bits=64;}
            const uint64_t mask=bits==64?UINT64_MAX:(uint64_t(1)<<bits)-1;
            auto value=next()&mask;const auto original=value;
            if(conversion=='d' || conversion=='i'){
                if(value&(uint64_t(1)<<(bits-1))){prefix="-";value=(uint64_t(0)-value)&mask;}
                else if(plus)prefix="+";else if(space)prefix=" ";
            }
            const unsigned base=conversion=='o'?8:(conversion=='x'||conversion=='X'||conversion=='p'?16:10);
            const char* alphabet=conversion=='X'?"0123456789ABCDEF":"0123456789abcdef";
            if(value || precision!=0)do{digits.push_back(alphabet[value%base]);value/=base;}while(value);
            std::reverse(digits.begin(),digits.end());
            if(precision>int(digits.size()))digits.insert(0,size_t(precision)-digits.size(),'0');
            if(conversion=='o' && alternate && (digits.empty() || digits[0]!='0'))digits.insert(0,1,'0');
            if(conversion=='p' || (alternate && original && (conversion=='x'||conversion=='X')))prefix=conversion=='X'?"0X":"0x";
        }else throw FormatUnsupported("Unsupported format conversion");
        const auto used=prefix.size()+digits.size(),padding=width>used?width-used:0;
        if(left){append(prefix);append(digits);append(std::string(padding,' '));}
        else if(zero && numeric && precision<0){append(prefix);append(std::string(padding,'0'));append(digits);}
        else {append(std::string(padding,' '));append(prefix);append(digits);}
    }
    if(capacity){
        const auto count=std::min<uint64_t>(capacity-1,output.size());
        std::string stored=output.substr(0,count);stored.push_back(0);
        memory.WriteBytes(destination,std::as_bytes(std::span(stored)));
    }
    return static_cast<int>(output.size());
}
}
