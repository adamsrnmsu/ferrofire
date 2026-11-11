// One-shot DNP3 TCP master on opendnp3 (Apache-2.0). Not interactive, unlike the stock example, so a harness can script it.
// usage: ff-dnp3-master HOST PORT LOCAL_ADDR REMOTE_ADDR [wait_s] [crob IDX CODE | ao IDX VALUE | select-crob IDX CODE]...
// It does an integrity poll (class 3/2/1/0) and then each command in order, printing decoded values and results to stdout.
#include <opendnp3/ConsoleLogger.h>
#include <opendnp3/DNP3Manager.h>
#include <opendnp3/channel/PrintingChannelListener.h>
#include <opendnp3/logging/LogLevels.h>
#include <opendnp3/master/DefaultMasterApplication.h>
#include <opendnp3/master/PrintingCommandResultCallback.h>
#include <opendnp3/master/PrintingSOEHandler.h>

#include <chrono>
#include <cstdlib>
#include <iostream>
#include <string>
#include <thread>

using namespace opendnp3;

static void pause(int ms) { std::this_thread::sleep_for(std::chrono::milliseconds(ms)); }

int main(int argc, char* argv[])
{
    if (argc < 5)
    {
        std::cerr << "usage: ff-dnp3-master HOST PORT LOCAL REMOTE [wait_s] [crob IDX CODE|select-crob IDX CODE|ao IDX VALUE]...\n";
        return 2;
    }
    const std::string host = argv[1];
    const uint16_t port = static_cast<uint16_t>(std::atoi(argv[2]));
    int i = 5;
    int wait_s = 2;
    if (i < argc && std::isdigit(static_cast<unsigned char>(argv[i][0]))) wait_s = std::atoi(argv[i++]);

    DNP3Manager manager(1, ConsoleLogger::Create());
    auto channel = manager.AddTCPClient("ff", levels::NORMAL | levels::ALL_APP_COMMS, ChannelRetry::Default(),
                                        {IPEndpoint(host, port)}, "0.0.0.0", PrintingChannelListener::Create());
    MasterStackConfig cfg;
    cfg.master.responseTimeout = TimeDuration::Seconds(3);
    cfg.master.disableUnsolOnStartup = true;
    cfg.link.LocalAddr = static_cast<uint16_t>(std::atoi(argv[3]));
    cfg.link.RemoteAddr = static_cast<uint16_t>(std::atoi(argv[4]));
    auto master = channel->AddMaster("ff", PrintingSOEHandler::Create(), DefaultMasterApplication::Create(), cfg);
    master->Enable();

    pause(1500); // link up
    master->ScanClasses(ClassField::AllClasses(), PrintingSOEHandler::Create());
    pause(wait_s * 1000);

    while (i + 2 < argc)
    {
        const std::string op = argv[i];
        const auto idx = static_cast<uint16_t>(std::atoi(argv[i + 1]));
        if (op == "crob" || op == "select-crob")
        {
            ControlRelayOutputBlock crob(static_cast<OperationType>(std::atoi(argv[i + 2])));
            if (op == "crob") master->DirectOperate(crob, idx, PrintingCommandResultCallback::Get());
            else master->SelectAndOperate(crob, idx, PrintingCommandResultCallback::Get());
        }
        else if (op == "ao")
        {
            master->DirectOperate(AnalogOutputFloat32(static_cast<float>(std::atof(argv[i + 2]))), idx, PrintingCommandResultCallback::Get());
        }
        else
        {
            std::cerr << "unknown op " << op << "\n";
            return 2;
        }
        i += 3;
        pause(1500);
    }
    master->ScanClasses(ClassField::AllClasses(), PrintingSOEHandler::Create());
    pause(wait_s * 1000);
    return 0;
}
