<?xml version="1.0" encoding="UTF-8"?>
<SCL xmlns="http://www.iec.ch/61850/2003/SCL" version="2007" revision="B" release="4">
  <Header id="ferrofire-iec61850-rtu"/>
  <Communication>
    <SubNetwork name="process">
      <ConnectedAP iedName="PUB" apName="P1">
        <GSE ldInst="LD0" cbName="gcbTrip">
          <Address><P type="MAC-Address">01-0C-CD-01-00-07</P><P type="APPID">0007</P></Address>
          <MinTime unit="s" multiplier="m">4</MinTime><MaxTime unit="s" multiplier="m">1000</MaxTime>
        </GSE>
      </ConnectedAP>
    </SubNetwork>
  </Communication>
  <IED name="PUB" manufacturer="go-iec61850" type="Relay">
    <AccessPoint name="P1"><Server>
      <LDevice inst="LD0">
        <LN0 lnClass="LLN0" inst="" lnType="LLN0_T">
          <DataSet name="dsTrip">
            <FCDA ldInst="LD0" lnClass="PTRC" lnInst="1" doName="Tr" daName="general" fc="ST"/>
            <FCDA ldInst="LD0" lnClass="PTRC" lnInst="1" doName="Tr" daName="q" fc="ST"/>
          </DataSet>
          <GSEControl name="gcbTrip" datSet="dsTrip" confRev="1" appID="PUB_Trip" type="GOOSE"/>
        </LN0>
        <LN lnClass="PTRC" inst="1" prefix="" lnType="PTRC_T"/>
      </LDevice>
    </Server></AccessPoint>
  </IED>
  <IED name="RTU1" manufacturer="ferrofire" type="RTU">
    <AccessPoint name="P1"><Server>
      <LDevice inst="LD0">
        <LN0 lnClass="LLN0" inst="" lnType="LLN0_T">
          <Inputs>
            <ExtRef iedName="PUB" ldInst="LD0" lnClass="PTRC" lnInst="1" doName="Tr" daName="general"
                    serviceType="GOOSE" srcLDInst="LD0" srcCBName="gcbTrip" intAddr="mirror"/>
          </Inputs>
        </LN0>
      </LDevice>
    </Server></AccessPoint>
  </IED>
  <DataTypeTemplates>
    <LNodeType id="LLN0_T" lnClass="LLN0"><DO name="Mod" type="ENC_T"/></LNodeType>
    <LNodeType id="PTRC_T" lnClass="PTRC"><DO name="Tr" type="ACT_T"/></LNodeType>
    <DOType id="ENC_T" cdc="ENC"><DA name="stVal" fc="ST" bType="Enum" type="Mod_E"/><DA name="q" fc="ST" bType="Quality"/></DOType>
    <DOType id="ACT_T" cdc="ACT"><DA name="general" fc="ST" bType="BOOLEAN"/><DA name="q" fc="ST" bType="Quality"/><DA name="t" fc="ST" bType="Timestamp"/></DOType>
    <EnumType id="Mod_E"><EnumVal ord="1">on</EnumVal></EnumType>
  </DataTypeTemplates>
</SCL>
