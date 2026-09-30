# \MarketDataApi

All URIs are relative to *http://localhost*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_expiry_calendar_v1**](MarketDataApi.md#get_expiry_calendar_v1) | **GET** /api/v1/expiries/{underlying} | 
[**get_expiry_calendar_v2**](MarketDataApi.md#get_expiry_calendar_v2) | **GET** /api/v2/expiries/{underlying} | 
[**get_implied_volatility_v1**](MarketDataApi.md#get_implied_volatility_v1) | **GET** /api/v1/iv | 
[**get_implied_volatility_v2**](MarketDataApi.md#get_implied_volatility_v2) | **GET** /api/v2/iv | 
[**get_option_chain_v1**](MarketDataApi.md#get_option_chain_v1) | **GET** /api/v1/chain | 
[**get_option_chain_v2**](MarketDataApi.md#get_option_chain_v2) | **GET** /api/v2/chain | 
[**get_option_price_v1**](MarketDataApi.md#get_option_price_v1) | **GET** /api/v1/price | 
[**get_option_price_v2**](MarketDataApi.md#get_option_price_v2) | **GET** /api/v2/price | 
[**get_protocol_stats_v1**](MarketDataApi.md#get_protocol_stats_v1) | **GET** /api/v1/stats | 
[**get_protocol_stats_v2**](MarketDataApi.md#get_protocol_stats_v2) | **GET** /api/v2/stats | 
[**get_spot_v1**](MarketDataApi.md#get_spot_v1) | **GET** /api/v1/spot | 
[**get_spot_v2**](MarketDataApi.md#get_spot_v2) | **GET** /api/v2/spot | 
[**subscribe_to_spot_v1**](MarketDataApi.md#subscribe_to_spot_v1) | **GET** /api/v1/ws/spot | 
[**subscribe_to_spot_v2**](MarketDataApi.md#subscribe_to_spot_v2) | **GET** /api/v2/ws/spot | 



## get_expiry_calendar_v1

> std::collections::HashMap<String, serde_json::Value> get_expiry_calendar_v1(underlying)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_expiry_calendar_v2

> std::collections::HashMap<String, serde_json::Value> get_expiry_calendar_v2(underlying)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_implied_volatility_v1

> std::collections::HashMap<String, serde_json::Value> get_implied_volatility_v1(underlying, strike, expiry_days, option_type, market_price)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |
**strike** | **f64** |  | [required] |
**expiry_days** | **f64** |  | [required] |
**option_type** | **String** |  | [required] |
**market_price** | **f64** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_implied_volatility_v2

> std::collections::HashMap<String, serde_json::Value> get_implied_volatility_v2(underlying, strike, expiry_days, option_type, market_price)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |
**strike** | **f64** |  | [required] |
**expiry_days** | **f64** |  | [required] |
**option_type** | **String** |  | [required] |
**market_price** | **f64** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_option_chain_v1

> std::collections::HashMap<String, serde_json::Value> get_option_chain_v1(underlying, expiry_days)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |
**expiry_days** | **f64** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_option_chain_v2

> std::collections::HashMap<String, serde_json::Value> get_option_chain_v2(underlying, expiry_days)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |
**expiry_days** | **f64** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_option_price_v1

> std::collections::HashMap<String, serde_json::Value> get_option_price_v1(underlying, strike, expiry_days, option_type)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |
**strike** | **f64** |  | [required] |
**expiry_days** | **f64** |  | [required] |
**option_type** | **String** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_option_price_v2

> std::collections::HashMap<String, serde_json::Value> get_option_price_v2(underlying, strike, expiry_days, option_type)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |
**strike** | **f64** |  | [required] |
**expiry_days** | **f64** |  | [required] |
**option_type** | **String** |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_protocol_stats_v1

> std::collections::HashMap<String, serde_json::Value> get_protocol_stats_v1()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_protocol_stats_v2

> std::collections::HashMap<String, serde_json::Value> get_protocol_stats_v2()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_spot_v1

> models::SpotResponseV1 get_spot_v1()


### Parameters

This endpoint does not need any parameter.

### Return type

[**models::SpotResponseV1**](SpotResponseV1.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_spot_v2

> models::SpotResponseV2 get_spot_v2()


### Parameters

This endpoint does not need any parameter.

### Return type

[**models::SpotResponseV2**](SpotResponseV2.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## subscribe_to_spot_v1

> subscribe_to_spot_v1()


WebSocket upgrade; the OpenAPI operation documents the handshake.

### Parameters

This endpoint does not need any parameter.

### Return type

 (empty response body)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: Not defined

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## subscribe_to_spot_v2

> subscribe_to_spot_v2()


WebSocket upgrade; the OpenAPI operation documents the handshake.

### Parameters

This endpoint does not need any parameter.

### Return type

 (empty response body)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: Not defined

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

