#pragma once
#include "guest_cpu.hpp"
#include <algorithm>
#include <memory>
#include <cstring>
#include <stdexcept>

namespace NativePS5 {
struct IoResult { uint64_t value=0; uint32_t error=0; };
// Single-threaded guest FIFOs and payload control descriptors.
// No host descriptors, networking or signal delivery.
class GuestDescriptors {
    struct Pipe {
        explicit Pipe(size_t capacity):bytes(capacity) {}
        std::vector<std::byte> bytes;
        size_t head=0,size=0;
        bool reader=true,writer=true;
        GuestMemory* kernel=nullptr;
        uint64_t controlAddress=0;
    };
    struct PacketControl {
        GuestMemory* kernel;
        uint64_t target=0;
    };
    struct Endpoint {
        std::shared_ptr<Pipe> pipe;
        bool writer=false;
        std::shared_ptr<PacketControl> control;
        bool selector=false;
    };
    std::array<Endpoint,256> endpoints{};
    Endpoint* Find(uint64_t descriptor) {
        if(descriptor<3 || descriptor-3>=endpoints.size()) return nullptr;
        auto& endpoint=endpoints[descriptor-3];
        return (endpoint.pipe || endpoint.control)?&endpoint:nullptr;
    }
    std::pair<size_t,size_t> FreePair() const {
        size_t first=endpoints.size(),second=endpoints.size();
        for(size_t i=0;i<endpoints.size();++i) if(!endpoints[i].pipe && !endpoints[i].control) {
            if(first==endpoints.size()) first=i; else {second=i;break;}
        }
        if(second==endpoints.size()) throw std::runtime_error("Guest descriptor limit reached");
        return {first,second};
    }
    IoResult KernelTransfer(Pipe& pipe,GuestMemory& user,uint64_t address,uint64_t count,bool write) {
        auto& kernel=*pipe.kernel;
        try {
            const auto control=pipe.controlAddress;
            kernel.ValidateAccess(control,24,true);
            const auto used=kernel.Load(control,4),in=kernel.Load(control+4,4),out=kernel.Load(control+8,4);
            const auto capacity=kernel.Load(control+12,4),buffer=kernel.Load(control+16,8);
            if(!capacity || capacity>0x40000000 || used>capacity || in>capacity || out>capacity || capacity>UINT64_MAX-buffer) return {0,22};
            if(write && !pipe.reader) return {0,32};
            const auto available=write?capacity-used:used;
            if(!available) return (!write && !pipe.writer)?IoResult{}:IoResult{0,35};
            if(write && count<=512 && count>available) return {0,35};
            const auto amount=std::min(count,available),position=(write?in:out)%capacity;
            const auto first=std::min(amount,capacity-position),second=amount-first;
            // Kernel data may not alias its own control block in this model.
            if(write && ((buffer+position<control+24 && control<buffer+position+first) ||
                         (second && buffer<control+24 && control<buffer+second))) return {0,22};
            user.ValidateAccess(address,amount,!write);
            kernel.ValidateAccess(buffer+position,first,write);kernel.ValidateAccess(buffer,second,write);
            std::vector<std::byte> copy(amount);
            if(write) {
                user.ReadBytes(address,copy);
                kernel.WriteBytes(buffer+position,std::span(copy).first(first));
                kernel.WriteBytes(buffer,std::span(copy).subspan(first));
            } else {
                kernel.ReadBytes(buffer+position,std::span(copy).first(first));
                kernel.ReadBytes(buffer,std::span(copy).subspan(first));
                user.WriteBytes(address,copy);
            }
            kernel.Store(control,write?used+amount:used-amount,4);
            kernel.Store(control+(write?4:8),(position+amount)%capacity,4);
            return {amount,0};
        }catch(const std::runtime_error&){return {0,14};}
    }
public:
    GuestDescriptors() = default;
    GuestDescriptors(const GuestDescriptors&) = delete;
    GuestDescriptors& operator=(const GuestDescriptors&) = delete;
    std::pair<uint32_t,uint32_t> CreatePipe(size_t capacity=16384) {
        if(capacity<512 || capacity>65536) throw std::invalid_argument("Pipe capacity outside supported range");
        const auto [first,second]=FreePair();
        auto pipe=std::make_shared<Pipe>(capacity);
        endpoints[first]={pipe,false,{},false};endpoints[second]={std::move(pipe),true,{},false};
        return {uint32_t(first+3),uint32_t(second+3)};
    }
    std::pair<uint32_t,uint32_t> CreateKernelPipe(GuestMemory& kernel,uint64_t controlAddress) {
        if(!controlAddress || controlAddress>UINT64_MAX-24) throw std::invalid_argument("Invalid kernel pipe control address");
        kernel.ValidateAccess(controlAddress,24,true);
        const auto [first,second]=FreePair();
        auto pipe=std::make_shared<Pipe>(0);pipe->kernel=&kernel;pipe->controlAddress=controlAddress;
        endpoints[first]={pipe,false,{},false};endpoints[second]={std::move(pipe),true,{},false};
        return {uint32_t(first+3),uint32_t(second+3)};
    }
    // Models the preconfigured control pair supplied to this payload ABI.
    // This is not socket creation or a general IPv6 network implementation.
    // The separate kernel address space must outlive these descriptors.
    std::pair<uint32_t,uint32_t> CreatePacketControlPair(GuestMemory& kernel) {
        const auto [first,second]=FreePair();
        auto control=std::make_shared<PacketControl>();control->kernel=&kernel;
        endpoints[first]={{},false,control,true};endpoints[second]={{},false,std::move(control),false};
        return {uint32_t(first+3),uint32_t(second+3)};
    }
    IoResult SetSocketOption(GuestMemory& memory,uint64_t descriptor,uint64_t level,uint64_t option,uint64_t address,uint64_t length) {
        auto endpoint=Find(descriptor);if(!endpoint) return {0,9};
        if(!endpoint->control) return {0,38};
        if(level!=41 || option!=46) return {0,42};
        if(length!=20) return {0,22};
        std::array<std::byte,20> value{};
        try {
            memory.ReadBytes(address,value);
            if(endpoint->selector) {
                uint64_t target=0;std::memcpy(&target,value.data(),8);
                endpoint->control->target=target;
            } else {
                if(!endpoint->control->target) return {0,14};
                endpoint->control->kernel->WriteBytes(endpoint->control->target,value);
            }
        }catch(const std::runtime_error&){return {0,14};}
        return {};
    }
    IoResult Close(uint64_t descriptor) {
        auto endpoint=Find(descriptor);if(!endpoint) return {0,9};
        if(endpoint->pipe) {if(endpoint->writer) endpoint->pipe->writer=false;else endpoint->pipe->reader=false;}
        *endpoint={};return {};
    }
    IoResult Read(GuestMemory& memory,uint64_t descriptor,uint64_t address,uint64_t count) {
        auto endpoint=Find(descriptor);if(!endpoint || !endpoint->pipe || endpoint->writer) return {0,9};
        if(count>1048576) return {0,22};
        if(!count) return {};
        auto& pipe=*endpoint->pipe;
        if(pipe.kernel) return KernelTransfer(pipe,memory,address,count,false);
        if(!pipe.size) return pipe.writer?IoResult{0,35}:IoResult{};
        const auto amount=std::min<uint64_t>(count,pipe.size);
        std::vector<std::byte> copy(amount);
        for(size_t i=0;i<amount;++i) copy[i]=pipe.bytes[(pipe.head+i)%pipe.bytes.size()];
        try {memory.WriteBytes(address,copy);}catch(const std::runtime_error&){return {0,14};}
        pipe.head=(pipe.head+amount)%pipe.bytes.size();pipe.size-=amount;
        return {amount,0};
    }
    IoResult Write(GuestMemory& memory,uint64_t descriptor,uint64_t address,uint64_t count) {
        auto endpoint=Find(descriptor);if(!endpoint || !endpoint->pipe || !endpoint->writer) return {0,9};
        if(count>1048576) return {0,22};
        if(!count) return {};
        auto& pipe=*endpoint->pipe;
        if(pipe.kernel) return KernelTransfer(pipe,memory,address,count,true);
        if(!pipe.reader) return {0,32};
        const auto space=pipe.bytes.size()-pipe.size;
        if(!space || (count<=512 && count>space)) return {0,35};
        const auto amount=std::min<uint64_t>(count,space);
        std::vector<std::byte> copy(amount);
        try {memory.ReadBytes(address,copy);}catch(const std::runtime_error&){return {0,14};}
        for(size_t i=0;i<amount;++i) pipe.bytes[(pipe.head+pipe.size+i)%pipe.bytes.size()]=copy[i];
        pipe.size+=amount;return {amount,0};
    }
};
}
